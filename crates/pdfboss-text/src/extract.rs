//! Content-op execution with full text state (Tm/Tlm, Tf, Tc, Tw, Tz, TL,
//! Ts), glyph advances, form XObject recursion, and the line/word layout
//! pass.

use crate::font::Font;
use pdfboss_core::content::{parse_content, Op, TextItem};
use pdfboss_core::{
    content_stream_data_with, page_content_with, AsyncObjectSource, Dict, Matrix, Object, Page,
    Point,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Maximum form-XObject recursion depth.
const MAX_FORM_DEPTH: usize = 16;

/// Maximum total form-XObject invocations per page. The depth cap alone
/// does not bound work: a chain of forms in which each level invokes the
/// next N times fans out to N^depth executions from a tiny file.
const MAX_FORM_INVOCATIONS: usize = 4096;

/// A positioned text run before layout: origin, advance end, device-space
/// size, and the font resource name that produced it.
pub struct RawSpan {
    pub text: String,
    pub x: f32,
    pub y: f32,
    /// Device-space x after the last glyph's advance.
    pub end_x: f32,
    pub size: f32,
    pub font: String,
}

/// What extraction could not read. Extraction is lenient the way rendering
/// is — content that will not fetch, decode, or parse yields no text rather
/// than an error — and this report is what keeps that leniency accountable:
/// an empty result with an empty report really is an empty page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractReport {
    /// Every piece of content that yielded no text, in encounter order.
    pub skipped: Vec<SkippedText>,
}

impl ExtractReport {
    /// True when every operator stream was fetched, parsed, and executed —
    /// nothing the extraction saw was left out of the result.
    pub fn is_complete(&self) -> bool {
        self.skipped.is_empty()
    }

    fn record(&mut self, kind: SkippedTextKind, cause: SkipCause) {
        self.skipped.push(SkippedText { kind, cause });
    }
}

/// One piece of content whose text (if any) is missing from the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedText {
    pub kind: SkippedTextKind,
    pub cause: SkipCause,
}

/// Which kind of operator stream was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkippedTextKind {
    /// The page's own `/Contents` — the whole page yielded no text.
    PageContents,
    /// A form XObject: its text and its entire subtree (nested forms
    /// included) are absent.
    Form,
    /// An XObject name that resolved to nothing usable; whether it held
    /// text cannot be known.
    XObject,
}

impl std::fmt::Display for SkippedTextKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SkippedTextKind::PageContents => "the page contents",
            SkippedTextKind::Form => "a form XObject",
            SkippedTextKind::XObject => "an XObject",
        })
    }
}

/// Why the stream was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipCause {
    /// A `/Filter` this library cannot run — including the two passthrough
    /// image codecs, whose still-encoded bytes no content parser may read
    /// (ISO 32000-1 7.4.9). A stream so labelled that nonetheless holds
    /// valid operators is skipped all the same: the label, not the bytes,
    /// is what decides, exactly as in rendering.
    UnsupportedFilter(String),
    /// The stream would not fetch or decode.
    Unreadable,
    /// The decoded bytes did not parse as content operators.
    Parse,
    /// The named resource is missing, or is not a stream.
    Missing,
    /// Form nesting depth or the per-page invocation budget was exhausted.
    LimitExceeded,
}

impl std::fmt::Display for SkipCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipCause::UnsupportedFilter(name) => write!(f, "unsupported filter /{name}"),
            SkipCause::Unreadable => f.write_str("stream would not read"),
            SkipCause::Parse => f.write_str("content would not parse"),
            SkipCause::Missing => f.write_str("missing resource"),
            SkipCause::LimitExceeded => f.write_str("form limit exceeded"),
        }
    }
}

/// Maps a fetch/decode error onto its cause, keeping the filter name — the
/// one detail a caller can act on (the same split rendering reports).
fn cause_for(error: &pdfboss_core::Error) -> SkipCause {
    match error {
        pdfboss_core::Error::UnsupportedFilter(name) => SkipCause::UnsupportedFilter(name.clone()),
        _ => SkipCause::Unreadable,
    }
}

/// Runs the page's content stream (and any form XObjects) and collects
/// every shown string as a [`RawSpan`], in emission order, along with the
/// report of what could not be read.
///
/// Lenient like rendering: a `/Contents` that will not fetch, decode, or
/// parse contributes no spans and one report entry, never an error — the
/// twin of `render_page_reporting`'s blank-page-with-a-report behavior.
///
/// The source is taken by value so that the returned future can be `'static`;
/// `page` is borrowed, which does not stand in the way, because a caller that
/// owns its page creates the borrow inside its own `async move` block. See
/// `pdfboss_core::source`'s "Signing a shared algorithm".
pub async fn page_spans_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> (Vec<RawSpan>, ExtractReport) {
    let mut report = ExtractReport::default();
    let content = match page_content_with(&src, page).await {
        Ok(content) => content,
        Err(e) => {
            report.record(SkippedTextKind::PageContents, cause_for(&e));
            Vec::new()
        }
    };
    let ops = match parse_content(&content) {
        Ok(ops) => ops,
        Err(_) => {
            report.record(SkippedTextKind::PageContents, SkipCause::Parse);
            Vec::new()
        }
    };
    let mut exec = Executor {
        src: &src,
        spans: Vec::new(),
        fallback: Arc::new(Font::fallback()),
        forms: 0,
        report,
    };
    let root = Frame::new(
        ops.into(),
        vec![Arc::new(page.resources.clone())],
        GState::new(),
        0,
    );
    exec.run(root).await;
    (exec.spans, exec.report)
}

/// The graphics-state parameters text extraction cares about. Saved and
/// restored by `q`/`Q`; carried into form XObjects.
#[derive(Clone)]
struct GState {
    ctm: Matrix,
    char_spacing: f32,
    word_spacing: f32,
    /// `Tz / 100`.
    horiz_scale: f32,
    leading: f32,
    rise: f32,
    font: Option<Arc<Font>>,
    font_name: String,
    size: f32,
}

impl GState {
    fn new() -> GState {
        GState {
            ctm: Matrix::identity(),
            char_spacing: 0.0,
            word_spacing: 0.0,
            horiz_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            font: None,
            font_name: String::new(),
            size: 0.0,
        }
    }
}

/// True when every matrix component is finite.
fn finite(m: &Matrix) -> bool {
    [m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite())
}

/// One suspended operator stream: what to execute, how far it has got, and
/// every piece of state that stream owns.
///
/// This is what the recursion into a form XObject became. A recursive `async fn`
/// has to box itself, and coercing that box to a `Send` future requires
/// `S: Sync` — which `Immediate<&Document>` cannot supply, so boxing would cost
/// the synchronous caller the shared implementation entirely. A stack of these
/// uses no `dyn`, so auto traits stay inferred per instantiation: the future is
/// `Send` over an asynchronous source and merely non-`Send` over a synchronous
/// one, which is correct for both.
struct Frame {
    /// Shared rather than owned so a handle can be held while the frame stack is
    /// pushed onto. Cloned once per visit to the frame, never per operator.
    ops: Arc<[Op]>,
    /// Resource dictionaries, innermost first. Owned, because a form's own
    /// `/Resources` is read out of its stream dictionary and so outlives nothing
    /// already on the stack.
    chain: Vec<Arc<Dict>>,
    /// Index of the next operator to execute.
    pc: usize,
    /// Form-XObject nesting depth, checked against `MAX_FORM_DEPTH`.
    depth: usize,
    gs: GState,
    /// The `q`/`Q` stack, per operator stream.
    saved: Vec<GState>,
    tm: Matrix,
    tlm: Matrix,
    /// Loaded fonts, per operator stream: every form invocation starts with an
    /// empty cache, as it did when each invocation was its own `run` call.
    fonts: HashMap<String, Arc<Font>>,
}

impl Frame {
    fn new(ops: Arc<[Op]>, chain: Vec<Arc<Dict>>, gs: GState, depth: usize) -> Frame {
        Frame {
            ops,
            chain,
            pc: 0,
            depth,
            gs,
            saved: Vec::new(),
            tm: Matrix::identity(),
            tlm: Matrix::identity(),
            fonts: HashMap::new(),
        }
    }
}

struct Executor<'a, S> {
    src: &'a S,
    spans: Vec<RawSpan>,
    fallback: Arc<Font>,
    /// Form-XObject invocations so far, checked against
    /// `MAX_FORM_INVOCATIONS`.
    forms: usize,
    /// What could not be read; carried out alongside the spans.
    report: ExtractReport,
}

impl<S: AsyncObjectSource> Executor<'_, S> {
    /// Looks up `/category/name` in the resource chain, innermost dictionary
    /// first (ISO 32000 §7.8.3).
    ///
    /// A nested form's own `/Resources` shadows its caller's for the names it
    /// defines and falls through for the ones it does not. This mirrors the
    /// renderer's `find_res`; the two crates must agree on which resource a
    /// name refers to, or the same file extracts different text than it
    /// paints.
    async fn find_res(&self, chain: &[Arc<Dict>], category: &str, name: &str) -> Option<Object> {
        for res in chain {
            let Some(cat) = res.get(category) else {
                continue;
            };
            let Ok(Object::Dict(dict)) = self.src.resolve(cat).await else {
                continue;
            };
            if let Some(value) = dict.get(name) {
                if let Ok(obj) = self.src.resolve(value).await {
                    return Some(obj);
                }
            }
        }
        None
    }

    /// Loads (with per-stream caching) the font resource `name` from the
    /// active resource chain, falling back to a default font.
    async fn font(
        &self,
        chain: &[Arc<Dict>],
        name: &str,
        cache: &mut HashMap<String, Arc<Font>>,
    ) -> Arc<Font> {
        if let Some(f) = cache.get(name) {
            return f.clone();
        }
        let resolved = self.find_res(chain, "Font", name).await;
        let loaded = match resolved.as_ref().and_then(|o| o.as_dict()) {
            Some(dict) => Arc::new(Font::load(self.src, dict).await),
            // No such resource, or one that is not a dictionary: the fallback
            // font keeps the text extractable rather than failing the page.
            None => self.fallback.clone(),
        };
        cache.insert(name.to_string(), loaded.clone());
        loaded
    }

    /// Executes an operator stream and every form XObject it invokes.
    ///
    /// A form invocation pushes a frame and leaves the inner loop, so the form
    /// runs to completion before its caller's next operator — the same
    /// depth-first order the recursive version emitted, which is what keeps span
    /// order identical. Nothing is owed on the way back out: unlike the
    /// renderer, this executor has no state to restore after a nested stream.
    async fn run(&mut self, root: Frame) {
        let mut frames = vec![root];
        // The running frame is held as a local rather than indexed in place, which
        // costs a move per visit and saves cloning the resource chain and the
        // graphics state on every `Do`. It is not a speed fix: reaching the frame
        // through `frames[top]` on each operator was measured against this shape
        // and the two are indistinguishable on `extract_text_warm_500_lines`.
        'frames: while let Some(mut frame) = frames.pop() {
            // Cloned once per visit rather than once per operator: the handle has
            // to outlive the `&mut frame` borrows below.
            let ops = Arc::clone(&frame.ops);
            while frame.pc < ops.len() {
                let op = &ops[frame.pc];
                frame.pc += 1;
                match op {
                    Op::SetFont(name, size) => {
                        let loaded = self.font(&frame.chain, &name.0, &mut frame.fonts).await;
                        frame.gs.font = Some(loaded);
                        frame.gs.font_name = name.0.clone();
                        frame.gs.size = *size;
                    }
                    Op::XObject(name) => {
                        let entered = self
                            .form_frame(&name.0, &frame.chain, &frame.gs, frame.depth)
                            .await;
                        if let Some(child) = entered {
                            // The caller goes back underneath its form: the form
                            // runs to completion, then the caller resumes at the
                            // operator after its `Do`. That is the depth-first
                            // order the recursive version emitted.
                            frames.push(frame);
                            frames.push(child);
                            continue 'frames;
                        }
                    }
                    op => self.step(&mut frame, op),
                }
            }
        }
    }

    /// Applies one operator that needs no I/O — everything except `Tf` and `Do`.
    /// `q`/`Q` and `cm` maintain the CTM; text operators maintain Tm/Tlm; shown
    /// strings become spans.
    fn step(&mut self, frame: &mut Frame, op: &Op) {
        match op {
            Op::Save => frame.saved.push(frame.gs.clone()),
            Op::Restore => {
                if let Some(saved) = frame.saved.pop() {
                    frame.gs = saved;
                }
            }
            Op::Concat(m) if finite(m) => frame.gs.ctm = m.concat(frame.gs.ctm),
            Op::BeginText => {
                frame.tm = Matrix::identity();
                frame.tlm = Matrix::identity();
            }
            Op::SetCharSpacing(v) => frame.gs.char_spacing = *v,
            Op::SetWordSpacing(v) => frame.gs.word_spacing = *v,
            Op::SetHorizScaling(v) => frame.gs.horiz_scale = v / 100.0,
            Op::SetLeading(v) => frame.gs.leading = *v,
            Op::SetTextRise(v) => frame.gs.rise = *v,
            Op::TextMove(tx, ty) => {
                frame.tlm = Matrix::translate(*tx, *ty).concat(frame.tlm);
                frame.tm = frame.tlm;
            }
            Op::TextMoveSetLeading(tx, ty) => {
                frame.gs.leading = -ty;
                frame.tlm = Matrix::translate(*tx, *ty).concat(frame.tlm);
                frame.tm = frame.tlm;
            }
            Op::SetTextMatrix(m) if finite(m) => {
                frame.tm = *m;
                frame.tlm = *m;
            }
            Op::TextNextLine => {
                frame.tlm = Matrix::translate(0.0, -frame.gs.leading).concat(frame.tlm);
                frame.tm = frame.tlm;
            }
            Op::ShowText(s) => self.emit(frame, s),
            Op::ShowTextAdjusted(items) => {
                for item in items {
                    match item {
                        TextItem::Str(s) => self.emit(frame, s),
                        TextItem::Offset(n) => {
                            let tx = -n / 1000.0 * frame.gs.size * frame.gs.horiz_scale;
                            if tx.is_finite() {
                                frame.tm = Matrix::translate(tx, 0.0).concat(frame.tm);
                            }
                        }
                    }
                }
            }
            Op::NextLineShowText(s) => {
                frame.tlm = Matrix::translate(0.0, -frame.gs.leading).concat(frame.tlm);
                frame.tm = frame.tlm;
                self.emit(frame, s);
            }
            Op::NextLineShowTextSpaced(aw, ac, s) => {
                frame.gs.word_spacing = *aw;
                frame.gs.char_spacing = *ac;
                frame.tlm = Matrix::translate(0.0, -frame.gs.leading).concat(frame.tlm);
                frame.tm = frame.tlm;
                self.emit(frame, s);
            }
            // Text render mode 3 (invisible) is still extracted, so `Tr` and
            // everything else is a no-op here.
            _ => {}
        }
    }

    /// Shows one string, appending the span it produces (if any) to the page.
    fn emit(&mut self, frame: &mut Frame, bytes: &[u8]) {
        if let Some(span) = self.show(&frame.gs, &mut frame.tm, bytes) {
            self.spans.push(span);
        }
    }

    /// Shows one string: decodes each code, advances the text matrix by
    /// `(w/1000·Tfs + Tc + Tw[code 32]) · Tz/100`, and returns a span whose
    /// origin is `(0, Ts)` under `Tm · CTM`. `None` when there is nothing worth
    /// recording — no decoded text, or an origin that is not finite.
    ///
    /// Returning the span rather than pushing it is what lets the active font be
    /// borrowed instead of cloned. `Tj` is the hottest operator on a text page and
    /// the handle is an `Arc` now, so cloning it there costs two atomic updates
    /// per shown string; borrowing `self.fallback` is only possible while nothing
    /// holds `&mut self.spans`.
    fn show(&self, gs: &GState, tm: &mut Matrix, bytes: &[u8]) -> Option<RawSpan> {
        let font: &Font = gs.font.as_deref().unwrap_or(&self.fallback);
        let start = tm.concat(gs.ctm);
        let origin = start.apply(Point { x: 0.0, y: gs.rise });
        // Device-space font size: the length of the text-space vertical
        // unit vector scaled by Tfs under Tm·CTM.
        let size = gs.size * (start.c * start.c + start.d * start.d).sqrt();
        let mut text = String::new();
        for code in font.codes(bytes) {
            font.decode_into(code, &mut text);
            let word = if font.is_space(code) {
                gs.word_spacing
            } else {
                0.0
            };
            let adv =
                (font.width(code) / 1000.0 * gs.size + gs.char_spacing + word) * gs.horiz_scale;
            if adv.is_finite() {
                *tm = Matrix::translate(adv, 0.0).concat(*tm);
            }
        }
        let end = tm.concat(gs.ctm).apply(Point { x: 0.0, y: gs.rise });
        (!text.is_empty() && origin.x.is_finite() && origin.y.is_finite()).then(|| RawSpan {
            text,
            x: origin.x,
            y: origin.y,
            end_x: end.x,
            size: if size.is_finite() { size } else { 0.0 },
            font: gs.font_name.clone(),
        })
    }

    /// Builds the frame for a form XObject invocation: its content stream, its
    /// own `/Resources` **prepended to** the caller's chain, and `/Matrix`
    /// prepended to the CTM — under a depth cap and a total-invocation budget.
    ///
    /// `None` on five ways out, each reported except the one that is normal:
    /// depth or budget exhausted (`LimitExceeded`); no such resource, or one
    /// that is not a stream (`Missing`); not a form — images and other
    /// XObjects carry no text, so this is silent; a fetch the chokepoint
    /// refuses (`UnsupportedFilter`, image codecs included) or that fails to
    /// decode (`Unreadable`); and content that will not parse (`Parse`). The
    /// invocation is counted before any of those checks, so a page of
    /// unreadable forms still exhausts its budget.
    async fn form_frame(
        &mut self,
        name: &str,
        chain: &[Arc<Dict>],
        gs: &GState,
        depth: usize,
    ) -> Option<Frame> {
        if depth >= MAX_FORM_DEPTH || self.forms >= MAX_FORM_INVOCATIONS {
            self.report
                .record(SkippedTextKind::Form, SkipCause::LimitExceeded);
            return None;
        }
        self.forms += 1;
        // Moved out, not cloned: `find_res` hands back an owned object, and
        // a form's stream carries its whole content body.
        let stream = match self.find_res(chain, "XObject", name).await {
            Some(Object::Stream(s)) => s,
            _ => {
                self.report
                    .record(SkippedTextKind::XObject, SkipCause::Missing);
                return None;
            }
        };
        // `/Subtype` may be indirect like any dictionary value (ISO 32000-1
        // 7.3.8.1): a direct name answers on the spot, a reference resolves.
        let is_form = match stream.dict.get("Subtype") {
            Some(Object::Name(n)) => n.0 == "Form",
            Some(indirect @ Object::Ref(_)) => self
                .src
                .resolve(indirect)
                .await
                .ok()
                .and_then(|o| o.as_name().map(|n| n.0 == "Form"))
                .unwrap_or(false),
            _ => false,
        };
        if !is_form {
            return None; // images and other XObjects carry no text
        }
        // Through the content chokepoint, not raw stream_data: a form whose
        // trailing /Filter is an image codec holds passthrough bytes, not
        // operators (see `content_stream_data_with`). The refusal is a
        // report entry, the same accountable skip rendering records.
        let data = match content_stream_data_with(self.src, &stream).await {
            Ok(data) => data,
            Err(e) => {
                self.report.record(SkippedTextKind::Form, cause_for(&e));
                return None;
            }
        };
        let ops = match parse_content(&data) {
            Ok(ops) => ops,
            Err(_) => {
                self.report.record(SkippedTextKind::Form, SkipCause::Parse);
                return None;
            }
        };
        // The form's own /Resources shadows the caller's for the names it
        // defines and falls through for the ones it does not, so it is
        // prepended rather than substituted. A form that declares
        // /Resources without a /Font (or without the /XObject naming a
        // nested form) still reaches the page's.
        let mut inner_chain: Vec<Arc<Dict>> = Vec::with_capacity(chain.len() + 1);
        if let Some(own) = self.own_resources(&stream.dict).await {
            inner_chain.push(Arc::new(own));
        }
        inner_chain.extend_from_slice(chain);

        let mut inner = gs.clone();
        if let Some(m) = self.form_matrix(&stream.dict).await {
            inner.ctm = m.concat(inner.ctm);
        }
        Some(Frame::new(ops.into(), inner_chain, inner, depth + 1))
    }

    /// A stream dictionary's own `/Resources`, when it has a usable one.
    async fn own_resources(&self, dict: &Dict) -> Option<Dict> {
        let obj = dict.get("Resources")?;
        self.src.resolve(obj).await.ok()?.as_dict().cloned()
    }

    /// Reads a `/Matrix` entry (six numbers) from a form XObject dictionary.
    async fn form_matrix(&self, dict: &Dict) -> Option<Matrix> {
        let obj = self.src.resolve(dict.get("Matrix")?).await.ok()?;
        let arr = obj.as_array()?;
        let mut v = [0.0f32; 6];
        for (slot, item) in v.iter_mut().zip(arr.iter()) {
            *slot = self.src.resolve(item).await.ok()?.as_f64()? as f32;
        }
        if arr.len() < 6 {
            return None;
        }
        let m = Matrix {
            a: v[0],
            b: v[1],
            c: v[2],
            d: v[3],
            e: v[4],
            f: v[5],
        };
        finite(&m).then_some(m)
    }
}

/// Fraction of the device font size a horizontal gap must exceed to read
/// as a word break. The ceiling is justified LaTeX's shrunk inter-word
/// glue — 0.17 em for Times-family fonts, and a hair less under a
/// compressed text matrix — and the floor is italic corrections and
/// kerns, which stay under 0.1 em; 0.25 em sat exactly on the nominal
/// Times space width and swallowed every shrunk line's spaces.
const WORD_GAP: f32 = 0.15;

/// Minimum column-candidate spans on a page before a gutter is looked for.
const COLUMN_MIN_SPANS: usize = 40;
/// Minimum spans and distinct baselines on each side of a candidate gutter.
const COLUMN_MIN_SIDE_SPANS: usize = 10;
const COLUMN_MIN_SIDE_LINES: usize = 6;
/// Each column must cover at least this fraction of the combined text
/// height — low enough that a final page whose right column ends early
/// still splits, high enough that a sidebar note does not.
const COLUMN_MIN_HEIGHT: f32 = 0.4;
/// Each column must also span at least this fraction of the text width:
/// a table's number or label column is far narrower than any genuine text
/// column, and splitting a table reads its rows column-major.
const COLUMN_MIN_SIDE_WIDTH: f32 = 0.25;
/// Minimum device-space gutter width, and the central band of the text
/// width the gutter's center must fall in.
const GUTTER_MIN_WIDTH: f32 = 6.0;
const GUTTER_BAND: std::ops::RangeInclusive<f32> = 0.25..=0.75;
/// A span wider than this fraction of the text width separates bands
/// (headings, footers) rather than belonging to either column.
const SEPARATOR_FRACTION: f32 = 0.5;
/// Occupancy-histogram resolution for gutter detection.
const GUTTER_BINS: usize = 128;

/// Groups spans into lines (baselines within `0.5 · size`), orders lines
/// top to bottom and spans left to right, inserts a space at horizontal
/// gaps wider than [`WORD_GAP`] times the size, and joins lines with `\n`.
/// A page with a clear two-column gutter reads column-major: full-width
/// separators split it into bands, and within each band the left column
/// flows before the right (see [`segments`]).
pub fn layout(spans: &[RawSpan]) -> String {
    let mut out = String::new();
    for segment in segments(spans) {
        if segment.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        flow(&segment, &mut out);
    }
    out
}

/// Lays one reading-order segment out into lines, appending to `out`.
fn flow(spans: &[&RawSpan], out: &mut String) {
    struct Line<'s> {
        y: f32,
        size: f32,
        spans: Vec<&'s RawSpan>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for &span in spans {
        let found = lines
            .iter_mut()
            .find(|line| (line.y - span.y).abs() <= 0.5 * line.size.max(span.size));
        match found {
            Some(line) => {
                line.size = line.size.max(span.size);
                line.spans.push(span);
            }
            None => lines.push(Line {
                y: span.y,
                size: span.size,
                spans: vec![span],
            }),
        }
    }
    lines.sort_by(|a, b| b.y.total_cmp(&a.y)); // top of page first
    for (i, line) in lines.iter_mut().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        line.spans.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut prev_end: Option<f32> = None;
        let mut prev_size = 0.0f32;
        for span in &line.spans {
            if let Some(end) = prev_end {
                let gap = span.x - end;
                if gap > WORD_GAP * prev_size.max(span.size) {
                    out.push(' ');
                }
            }
            out.push_str(&span.text);
            prev_end = Some(span.end_x);
            prev_size = span.size;
        }
    }
}

/// The page's spans in reading order, cut into segments.
///
/// Detects a two-column layout by x-occupancy: full-width spans are set
/// aside as band separators, the rest are histogrammed, and the widest
/// empty run whose center sits in the middle of the text width is the
/// gutter candidate. The split only happens when both sides look like
/// real columns (enough spans, enough distinct baselines, enough shared
/// height) — anything less reads top-to-bottom as one segment, which is
/// exactly the old behavior.
fn segments(spans: &[RawSpan]) -> Vec<Vec<&RawSpan>> {
    let whole = || vec![spans.iter().collect::<Vec<&RawSpan>>()];
    let mut x_min = f32::INFINITY;
    let mut x_max = f32::NEG_INFINITY;
    for span in spans {
        x_min = x_min.min(span.x.min(span.end_x));
        x_max = x_max.max(span.x.max(span.end_x));
    }
    let width = x_max - x_min;
    if !width.is_finite() || width <= 0.0 {
        return whole();
    }
    let (separators, body): (Vec<&RawSpan>, Vec<&RawSpan>) = spans
        .iter()
        .partition(|s| (s.end_x - s.x).abs() > SEPARATOR_FRACTION * width);
    if body.len() < COLUMN_MIN_SPANS {
        return whole();
    }
    // Two-column flow lives on portrait-shaped text blocks. A block wider
    // than it is tall is a slide or a table sheet, where a lone lane is a
    // cell boundary, not a gutter.
    let (body_lo, body_hi) = y_extent(&body);
    if body_hi - body_lo <= width {
        return whole();
    }

    let mut occupied = [false; GUTTER_BINS];
    let scale = GUTTER_BINS as f32 / width;
    for span in &body {
        let lo = ((span.x.min(span.end_x) - x_min) * scale).floor().max(0.0) as usize;
        let hi = ((span.x.max(span.end_x) - x_min) * scale).ceil() as usize;
        for bin in occupied.iter_mut().take(hi.min(GUTTER_BINS)).skip(lo) {
            *bin = true;
        }
    }
    // Exactly one wide interior lane is a gutter; several are the cell
    // columns of a data table, whose rows must keep reading left to right.
    let gaps = wide_gaps(&occupied, scale);
    let [gutter] = gaps.as_slice() else {
        return whole();
    };
    let center = (gutter.start + gutter.end) as f32 / 2.0 / GUTTER_BINS as f32;
    if !GUTTER_BAND.contains(&center) {
        return whole();
    }
    let cut = x_min + (gutter.start + gutter.end) as f32 / 2.0 / scale;

    let (left, right): (Vec<&RawSpan>, Vec<&RawSpan>) =
        body.iter().partition(|s| s.x.max(s.end_x) <= cut);
    if !column_shaped(&left) || !column_shaped(&right) {
        return whole();
    }
    if x_span(&left) < COLUMN_MIN_SIDE_WIDTH * width
        || x_span(&right) < COLUMN_MIN_SIDE_WIDTH * width
    {
        return whole();
    }
    let (left_lo, left_hi) = y_extent(&left);
    let (right_lo, right_hi) = y_extent(&right);
    let height = left_hi.max(right_hi) - left_lo.min(right_lo);
    if height <= 0.0
        || left_hi - left_lo < COLUMN_MIN_HEIGHT * height
        || right_hi - right_lo < COLUMN_MIN_HEIGHT * height
    {
        return whole();
    }

    // Bands run top to bottom; each separator line closes the columns
    // above it and reads between them and the columns below.
    let mut cuts: Vec<f32> = separators.iter().map(|s| s.y).collect();
    cuts.sort_by(|a, b| b.total_cmp(a));
    cuts.dedup();
    let mut out: Vec<Vec<&RawSpan>> = Vec::new();
    let mut top = f32::INFINITY;
    for &sep_y in &cuts {
        push_band(&left, &right, top, sep_y, &mut out);
        out.push(
            separators
                .iter()
                .filter(|s| s.y == sep_y)
                .copied()
                .collect(),
        );
        top = sep_y;
    }
    push_band(&left, &right, top, f32::NEG_INFINITY, &mut out);
    out
}

/// Pushes one band's columns — the spans with baseline in `(bottom, top]` —
/// left side first.
fn push_band<'s>(
    left: &[&'s RawSpan],
    right: &[&'s RawSpan],
    top: f32,
    bottom: f32,
    out: &mut Vec<Vec<&'s RawSpan>>,
) {
    for side in [left, right] {
        out.push(
            side.iter()
                .filter(|s| s.y <= top && s.y > bottom)
                .copied()
                .collect(),
        );
    }
}

/// Every interior run of empty bins at least [`GUTTER_MIN_WIDTH`] wide in
/// device space, as half-open bin ranges. Runs touching either edge are
/// margins, not lanes, and are not reported.
fn wide_gaps(occupied: &[bool; GUTTER_BINS], scale: f32) -> Vec<std::ops::Range<usize>> {
    let mut gaps = Vec::new();
    let mut run_start: Option<usize> = None;
    for i in 0..=GUTTER_BINS {
        let filled = i == GUTTER_BINS || occupied[i];
        match (filled, run_start.take()) {
            (false, None) => run_start = Some(i),
            (false, Some(start)) => run_start = Some(start),
            (true, Some(start)) => {
                let interior = start > 0 && i < GUTTER_BINS;
                if interior && (i - start) as f32 / scale >= GUTTER_MIN_WIDTH {
                    gaps.push(start..i);
                }
            }
            (true, None) => {}
        }
    }
    gaps
}

/// True when a gutter side has enough spans on enough distinct baselines
/// to be a text column rather than a stray cluster.
fn column_shaped(spans: &[&RawSpan]) -> bool {
    if spans.len() < COLUMN_MIN_SIDE_SPANS {
        return false;
    }
    let mut baselines: Vec<i32> = spans.iter().map(|s| s.y.round() as i32).collect();
    baselines.sort_unstable();
    baselines.dedup();
    baselines.len() >= COLUMN_MIN_SIDE_LINES
}

/// Lowest and highest baseline of a span set.
fn y_extent(spans: &[&RawSpan]) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for span in spans {
        lo = lo.min(span.y);
        hi = hi.max(span.y);
    }
    (lo, hi)
}

/// Horizontal extent of a span set.
fn x_span(spans: &[&RawSpan]) -> f32 {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for span in spans {
        lo = lo.min(span.x.min(span.end_x));
        hi = hi.max(span.x.max(span.end_x));
    }
    hi - lo
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{block_on, Document, Immediate};
    use pdfboss_testkit::doc_with_graphics;

    /// The synchronous spans accessor. Production has no use for one — the public
    /// entry points in `lib.rs` wrap [`page_spans_with`] themselves — but it is
    /// the same `block_on` over `Immediate`, so every test below still asserts on
    /// exactly what a synchronous caller receives. The report is asserted
    /// complete: no test here expects to lose content.
    fn page_spans(doc: &Document, page: &Page) -> Vec<RawSpan> {
        let (spans, report) = block_on(page_spans_with(Immediate(doc), page));
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        spans
    }

    /// Extracted, laid-out text of a one-page document with `content` as
    /// its raw content stream (12pt /F1 with default widths of 500).
    fn text_of(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        layout(&page_spans(&doc, &page))
    }

    /// Raw spans of the same setup.
    fn spans_of(content: &str) -> Vec<RawSpan> {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        page_spans(&doc, &page)
    }

    #[test]
    fn two_td_lines_become_newline() {
        let text = text_of("BT /F1 12 Tf 72 720 Td (Line one) Tj 0 -20 Td (Line two) Tj ET");
        assert_eq!(text, "Line one\nLine two");
    }

    #[test]
    fn tj_offset_space_thresholds() {
        // -300/1000 * 12 = 3.6 > 0.15 * 12 -> space.
        assert_eq!(
            text_of("BT /F1 12 Tf 72 720 Td [(A) -300 (B)] TJ ET"),
            "A B"
        );
        // -50/1000 * 12 = 0.6 -> no space.
        assert_eq!(text_of("BT /F1 12 Tf 72 720 Td [(A) -50 (B)] TJ ET"), "AB");
    }

    /// Justified LaTeX shrinks inter-word glue below the font's nominal
    /// space width: a Times word gap of 251/1000 em under a slightly
    /// compressed text matrix lands just under 0.25 em in device space,
    /// and a 0.25·size gap threshold reads the whole line as one word.
    #[test]
    fn shrunk_justified_word_gaps_still_become_spaces() {
        let text = text_of("BT /F1 12 Tf 0.993 0 0 1 72 720 Tm [(We) -251 (would)] TJ ET");
        assert_eq!(text, "We would");
    }

    #[test]
    fn word_spacing_applies_to_code_32_only() {
        // 'a b' = three codes at 6.0 each; Tw 5 fires once (the space).
        let spans = spans_of("BT /F1 12 Tf 5 Tw 72 720 Td (a b) Tj ET");
        assert_eq!(spans.len(), 1);
        assert!((spans[0].end_x - 95.0).abs() < 1e-3, "{}", spans[0].end_x);
    }

    #[test]
    fn cm_and_q_q_track_ctm() {
        let spans = spans_of(
            "q 1 0 0 1 100 0 cm BT /F1 12 Tf 0 720 Td (X) Tj ET Q \
             BT /F1 12 Tf 0 700 Td (Y) Tj ET",
        );
        assert_eq!(spans.len(), 2);
        assert!((spans[0].x - 100.0).abs() < 1e-3);
        assert!((spans[1].x - 0.0).abs() < 1e-3);
    }

    #[test]
    fn horizontal_scaling_stretches_advances() {
        let spans = spans_of("BT /F1 12 Tf 200 Tz 72 720 Td (AB) Tj ET");
        // 2 glyphs * 6.0 * 200% = 24.
        assert!((spans[0].end_x - 96.0).abs() < 1e-3, "{}", spans[0].end_x);
    }

    #[test]
    fn text_rise_shifts_baseline() {
        let spans = spans_of("BT /F1 12 Tf 72 720 Td 5 Ts (R) Tj ET");
        assert!((spans[0].y - 725.0).abs() < 1e-3);
    }

    #[test]
    fn invisible_render_mode_still_extracted() {
        assert_eq!(
            text_of("BT /F1 12 Tf 3 Tr 72 720 Td (ghost) Tj ET"),
            "ghost"
        );
    }

    #[test]
    fn leading_and_t_star_and_quote() {
        let text = text_of("BT /F1 12 Tf 14 TL 72 720 Td (a) Tj T* (b) Tj (c) ' ET");
        assert_eq!(text, "a\nb\nc");
        let spans = spans_of("BT /F1 12 Tf 14 TL 72 720 Td (a) Tj T* (b) Tj ET");
        assert!((spans[1].y - 706.0).abs() < 1e-3);
    }

    #[test]
    fn tm_positions_directly_and_bt_resets() {
        let spans = spans_of("BT /F1 12 Tf 1 0 0 1 300 100 Tm (m) Tj ET BT /F1 12 Tf (o) Tj ET");
        assert!((spans[0].x - 300.0).abs() < 1e-3);
        assert!((spans[0].y - 100.0).abs() < 1e-3);
        // Second BT starts from identity again.
        assert!((spans[1].x - 0.0).abs() < 1e-3);
        assert!((spans[1].y - 0.0).abs() < 1e-3);
    }

    #[test]
    fn tm_scale_sets_device_size() {
        let spans = spans_of("BT /F1 1 Tf 12 0 0 12 72 720 Tm (s) Tj ET");
        assert!((spans[0].size - 12.0).abs() < 1e-3);
    }

    #[test]
    fn layout_orders_spans_left_to_right() {
        let text = text_of(
            "BT /F1 12 Tf 200 720 Td (world) Tj ET \
             BT /F1 12 Tf 72 720 Td (hello) Tj ET",
        );
        assert_eq!(text, "hello world");
    }

    #[test]
    fn empty_content_yields_no_spans() {
        assert!(spans_of("").is_empty());
        assert_eq!(text_of("BT ET"), "");
    }

    /// One line of four word spans at `x` on baseline `y`, TJ-separated the
    /// way justified text is.
    fn column_line(x: u32, y: u32, tag: &str) -> String {
        format!(
            "BT /F1 12 Tf {x} {y} Td [({tag}a) -400 ({tag}b) -400 ({tag}c) -400 ({tag}d)] TJ ET "
        )
    }

    /// A dense two-column body: `lines` baselines per column, left column at
    /// x=72, right at x=240.
    fn two_column_content(lines: u32) -> String {
        (0..lines)
            .flat_map(|i| {
                let y = 720 - i * 14;
                [
                    column_line(72, y, &format!("L{i}")),
                    column_line(240, y, &format!("R{i}")),
                ]
            })
            .collect()
    }

    /// A page with a clear central gutter reads column-major: the whole left
    /// column, then the whole right column — not line-by-line across both.
    #[test]
    fn two_column_page_reads_column_major() {
        let text = text_of(&two_column_content(25));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0], "L0a L0b L0c L0d");
        assert_eq!(lines[24], "L24a L24b L24c L24d");
        assert_eq!(lines[25], "R0a R0b R0c R0d");
        assert_eq!(lines[49], "R24a R24b R24c R24d");
    }

    /// A full-width line above the columns is a band separator: it reads
    /// first, and the columns below it still read column-major.
    #[test]
    fn full_width_heading_reads_before_both_columns() {
        let content = format!(
            "BT /F1 12 Tf 72 760 Td (A quite wide heading spanning both text columns here) Tj ET {}",
            two_column_content(25)
        );
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "A quite wide heading spanning both text columns here"
        );
        assert_eq!(lines[1], "L0a L0b L0c L0d");
        assert_eq!(lines[26], "R0a R0b R0c R0d");
    }

    /// Two clusters with too few lines to be columns keep the plain
    /// top-to-bottom, left-to-right order.
    #[test]
    fn sparse_clusters_do_not_split_into_columns() {
        let text = text_of(&two_column_content(3));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "L0a L0b L0c L0d R0a R0b R0c R0d");
    }

    /// A text block wider than it is tall is a slide or a table sheet, not
    /// flowing two-column prose: its lone lane is a cell boundary. Modeled
    /// on a landscape product-overview slide that regressed when the gutter
    /// split first landed.
    #[test]
    fn wide_flat_block_does_not_split() {
        let content: String = (0..12)
            .flat_map(|i| {
                let y = 720 - i * 14;
                [
                    format!("BT /F1 12 Tf 72 {y} Td [(Stagename{i}) -400 (functionaa) -400 (listing)] TJ ET "),
                    format!("BT /F1 12 Tf 400 {y} Td [(Explanation{i}) -400 (of) -400 (the) -400 (feature)] TJ ET "),
                ]
            })
            .collect();
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 12);
        assert!(lines[0].starts_with("Stagename0 functionaa listing Explanation0"));
    }

    /// A data table has several full-height empty lanes between its cell
    /// columns where two-column prose has exactly one gutter; picking the
    /// widest lane of a table and splitting there reads the rows
    /// column-major. Modeled on a seven-column registration-results table
    /// that regressed when the gutter split first landed.
    #[test]
    fn multi_lane_table_does_not_split() {
        let content: String = (0..30)
            .map(|i| {
                let y = 720 - i * 14;
                format!(
                    "BT /F1 12 Tf 72 {y} Td (Rowname{i}) Tj ET \
                     BT /F1 12 Tf 200 {y} Td (12345) Tj ET \
                     BT /F1 12 Tf 330 {y} Td (678) Tj ET \
                     BT /F1 12 Tf 430 {y} Td (90) Tj ET "
                )
            })
            .collect();
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 30);
        assert_eq!(lines[0], "Rowname0 12345 678 90");
    }

    /// A table's narrow number column beside a wide text column is not a
    /// two-column layout: rows keep reading left to right. Modeled on a
    /// party-list table that regressed when the gutter split first landed.
    #[test]
    fn narrow_table_column_does_not_split() {
        let content: String = (0..30)
            .map(|i| {
                let y = 720 - i * 14;
                format!(
                    "BT /F1 12 Tf 72 {y} Td (1{i}) Tj ET \
                     BT /F1 12 Tf 300 {y} Td [(Partyaa) -300 (Nameebb) -300 (Row{i})] TJ ET "
                )
            })
            .collect();
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 30);
        assert_eq!(lines[0], "10 Partyaa Nameebb Row0");
    }

    #[test]
    fn form_xobject_fanout_is_bounded() {
        use pdfboss_testkit::PdfBuilder;
        // A chain of 6 forms in which each level invokes the next 8
        // times: bounded only by depth this executes 8^5 = 32768 leaf
        // forms (and grows exponentially with chain length), so the
        // total-invocation budget must cut it off.
        let chain = 6u32;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /XObject << /X 10 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/X Do");
        for i in 0..chain {
            let num = 10 + i;
            if i + 1 < chain {
                let dict = format!(
                    "/Type /XObject /Subtype /Form \
                     /Resources << /XObject << /X {} 0 R >> >>",
                    num + 1
                );
                b.stream(num, &dict, "/X Do ".repeat(8).as_bytes());
            } else {
                b.stream(
                    num,
                    "/Type /XObject /Subtype /Form",
                    b"BT /F1 12 Tf 72 720 Td (L) Tj ET",
                );
            }
        }
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        // Raw call: exhausting the budget is this test's point, so the
        // report is legitimately incomplete here.
        let (spans, report) = block_on(page_spans_with(Immediate(&doc), &page));
        assert!(!spans.is_empty()); // nested forms still extract text
        assert!(
            spans.len() <= MAX_FORM_INVOCATIONS,
            "fan-out not bounded: {} spans",
            spans.len()
        );
        assert!(
            report
                .skipped
                .iter()
                .all(|s| s.cause == SkipCause::LimitExceeded),
            "only the budget may cut this page short: {report:?}"
        );
        assert!(!report.is_complete(), "the cut-off must be visible");
    }

    /// Emission order is depth-first and in stream order: a form's spans land
    /// between the spans of the operators either side of its `Do`, at every
    /// level of nesting.
    ///
    /// This is what an explicit frame stack most easily gets wrong, and until now
    /// nothing tested it. `form_xobject_recursion` invokes its only form as the
    /// last operator on the page, so it cannot see a form's spans arriving late;
    /// `form_xobject_fanout_is_bounded` emits the same string from every leaf, so
    /// it cannot see siblings arriving reversed. Both stay green under a stack
    /// that defers children to the end.
    ///
    /// `page_spans` rather than `text_of`, because layout sorts by position and
    /// would hide the very thing being asserted.
    #[test]
    fn form_spans_are_emitted_where_the_do_appears() {
        use pdfboss_testkit::PdfBuilder;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> \
             /XObject << /Fa 6 0 R /Fi 7 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf 72 720 Td (A) Tj ET /Fa Do BT /F1 12 Tf 72 660 Td (E) Tj ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        // The outer form shows a string, descends, then shows another: its second
        // span must follow the nested form's.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792]",
            b"BT /F1 12 Tf 72 700 Td (B) Tj ET /Fi Do BT /F1 12 Tf 72 680 Td (D) Tj ET",
        );
        b.stream(
            7,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792]",
            b"BT /F1 12 Tf 72 690 Td (C) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = page_spans(&doc, &page);
        let order: Vec<&str> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(order, ["A", "B", "C", "D", "E"]);
    }

    /// A loaded font and the state that carries it must both be shareable across
    /// threads: the shared asynchronous implementation is driven on a runtime
    /// free to move its future between them, and `Arc<T>` is `Send` only when
    /// `T` is `Send + Sync`.
    ///
    /// Nothing in this crate has interior mutability, so this holds as soon as
    /// the handle is an `Arc`. The assertion exists to stop a later `Rc` or
    /// `RefCell` taking it away silently — which is exactly how the renderer's
    /// glyph cache came to block a spawnable future.
    #[test]
    fn loaded_fonts_are_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Font>();
        assert_send_sync::<Arc<Font>>();
        assert_send_sync::<GState>();
    }
}
