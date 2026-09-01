//! Content-op execution with full text state (Tm/Tlm, Tf, Tc, Tw, Tz, TL,
//! Ts), glyph advances, and form XObject recursion.

use crate::font::Font;
use crate::{ReadingOrder, Ruling, TextSpan};
use pdfboss_core::content::{ContentOps, Op, TextItem};
use pdfboss_core::{
    content_stream_data_with, page_content_with, AsyncObjectSource, Dict, FastMap, MarkedContentId,
    Matrix, Name, ObjRef, Object, OcState, Page, Point, Rect, StructureTree,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Maximum form-XObject recursion depth.
const MAX_FORM_DEPTH: usize = 16;

/// Maximum total form-XObject invocations per page. The depth cap alone
/// does not bound work: a chain of forms in which each level invokes the
/// next N times fans out to N^depth executions from a tiny file.
const MAX_FORM_INVOCATIONS: usize = 4096;

/// Maximum device-space cross-axis deviation over a path segment for it to
/// count as axis-aligned after the CTM.
const RULING_AXIS_EPSILON: f32 = 0.5;

/// Minimum device-space length of a ruling. Shorter marks (tick marks,
/// dashes of glyph decoration) are not table structure.
const RULING_MIN_LENGTH: f32 = 8.0;

/// Maximum thin dimension of a filled rectangle that reads as a drawn line;
/// anything fatter is a shaded box, not a ruling.
const RULING_MAX_FILL_THICKNESS: f32 = 3.0;

/// What extraction could not read. Extraction is lenient the way rendering
/// is — content that will not fetch, decode, or parse yields no text rather
/// than an error — and this report is what keeps that leniency accountable:
/// an empty result with an empty report really is an empty page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractReport {
    /// Every piece of content that yielded no text, in encounter order.
    pub skipped: Vec<SkippedText>,
    /// Content the document's optional-content configuration turns off
    /// (ISO 32000-1 §8.11): one count per `BDC /OC` span whose own
    /// membership evaluated hidden and per form XObject with a hidden
    /// `/OC` entry — a counter rather than entries, so a layer-heavy page
    /// cannot balloon `skipped`. Configured behavior, not a loss:
    /// [`ExtractReport::is_complete`] ignores it.
    pub hidden: u64,
    /// The order the spans came out in: the order requested, except that
    /// [`ReadingOrder::StructureTree`] falls back to
    /// [`ReadingOrder::Content`] on a page the structure tree does not reach
    /// (no tree, no parent-tree entry, no marked content), so the two read
    /// the same there.
    pub order: ReadingOrder,
}

impl ExtractReport {
    /// True when every operator stream was fetched, parsed, and executed —
    /// nothing the extraction saw was left out of the result. Content the
    /// document's optional-content configuration hides (`hidden`) was read
    /// and deliberately excluded, so it does not count against this.
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
    /// A Type0 font whose `/Encoding` CMap did not resolve (an unknown
    /// name, or predefined data this build does not carry): its text is
    /// still extracted under the Identity guess, which usually reads as
    /// U+FFFD.
    FontEncoding,
}

impl std::fmt::Display for SkippedTextKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SkippedTextKind::PageContents => "the page contents",
            SkippedTextKind::Form => "a form XObject",
            SkippedTextKind::XObject => "an XObject",
            SkippedTextKind::FontEncoding => "a font's CMap encoding",
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

/// Loaded fonts shared across page extractions of one document, keyed by the
/// font dictionary's object reference.
///
/// The name→font binding is resource-scoped — `/F1` in one form and `/F1` in
/// the page resources may be different fonts — so names are never keys here.
/// The reference is: within a document (and its forks, which share the same
/// bytes) an object reference resolves to the same dictionary every time, and
/// loading that dictionary yields the same font. A font held as a direct
/// dictionary has no reference and is never cached here.
///
/// `Send + Sync`, so one cache may serve every worker of a parallel
/// page walk; the executor consults it at most once per page per distinct
/// font reference.
#[derive(Default)]
pub struct FontCache {
    fonts: Mutex<HashMap<ObjRef, Arc<Font>>>,
}

impl FontCache {
    fn get(&self, r: ObjRef) -> Option<Arc<Font>> {
        self.fonts.lock().unwrap().get(&r).cloned()
    }

    /// Stores `font` under `r`, keeping (and returning) an already-present
    /// entry: concurrent workers may load the same font twice, and the copies
    /// are interchangeable, so the first one in wins.
    fn insert(&self, r: ObjRef, font: Arc<Font>) -> Arc<Font> {
        self.fonts.lock().unwrap().entry(r).or_insert(font).clone()
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
/// every shown string as a [`TextSpan`] and every axis-aligned drawn line
/// as a [`Ruling`], each in emission order, along with the report of what
/// could not be read.
///
/// Lenient like rendering: a `/Contents` that will not fetch, decode, or
/// parse contributes no spans and one report entry, never an error — the
/// twin of `render_page_reporting`'s blank-page-with-a-report behavior.
///
/// The source is taken by value so that the returned future can be `'static`;
/// `page` is borrowed, which does not stand in the way, because a caller that
/// owns its page creates the borrow inside its own `async move` block. See
/// `pdfboss_core::source`'s "Signing a shared algorithm".
pub async fn page_spans_and_rulings_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    fonts: Option<&FontCache>,
    oc: Option<&OcState>,
    structure: Option<&StructureTree>,
    order: ReadingOrder,
) -> (Vec<TextSpan>, Vec<Ruling>, ExtractReport) {
    match order {
        ReadingOrder::Content => content_order_with(src, page, fonts, oc).await,
        ReadingOrder::StructureTree => {
            structure_tree_order_with(src, page, fonts, oc, structure).await
        }
        ReadingOrder::Geometric => geometric_order_with(src, page, fonts, oc).await,
    }
}

/// The page's spans as the content stream emits them: the walk with no
/// marked-content recording compiled in.
async fn content_order_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    fonts: Option<&FontCache>,
    oc: Option<&OcState>,
) -> (Vec<TextSpan>, Vec<Ruling>, ExtractReport) {
    let (spans, rulings, report, Ignored) =
        walk_with::<S, Ignored>(&src, page, fonts, oc, ReadingOrder::Content).await;
    (spans, rulings, report)
}

/// The same spans as [`content_order_with`], tagged for geometric layout:
/// ordering by position is the layout stage's work, the extraction is the
/// content-order walk.
async fn geometric_order_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    fonts: Option<&FontCache>,
    oc: Option<&OcState>,
) -> (Vec<TextSpan>, Vec<Ruling>, ExtractReport) {
    let (spans, rulings, report, Ignored) =
        walk_with::<S, Ignored>(&src, page, fonts, oc, ReadingOrder::Geometric).await;
    (spans, rulings, report)
}

/// The page's spans in structure-tree order: the walk that records each
/// span's marked-content sequence, then the reorder by the tree's ranks.
/// With no tree, or a page the tree does not reach, the spans stay in
/// content order and the report says so.
async fn structure_tree_order_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    fonts: Option<&FontCache>,
    oc: Option<&OcState>,
    structure: Option<&StructureTree>,
) -> (Vec<TextSpan>, Vec<Ruling>, ExtractReport) {
    let Some(tree) = structure else {
        return content_order_with(src, page, fonts, oc).await;
    };
    let (mut spans, rulings, mut report, recorded) =
        walk_with::<S, Recorded>(&src, page, fonts, oc, ReadingOrder::Content).await;
    if structure_order(&src, tree, page, &mut spans, &recorded.ids).await {
        report.order = ReadingOrder::StructureTree;
    }
    (spans, rulings, report)
}

/// One page walk, compiled once per [`MarkedContent`] strategy: the
/// executor over the page's content and every form it invokes, then the
/// underline and strikethrough pass over the rulings it drew.
async fn walk_with<S: AsyncObjectSource, M: MarkedContent>(
    src: &S,
    page: &Page,
    fonts: Option<&FontCache>,
    oc: Option<&OcState>,
    order: ReadingOrder,
) -> (Vec<TextSpan>, Vec<Ruling>, ExtractReport, M) {
    let mut report = ExtractReport {
        order,
        ..ExtractReport::default()
    };
    let content = match page_content_with(src, page).await {
        Ok(content) => content,
        Err(e) => {
            report.record(SkippedTextKind::PageContents, cause_for(&e));
            Vec::new()
        }
    };
    let mut exec = Executor {
        src,
        spans: Vec::new(),
        rulings: Vec::new(),
        fallback: Arc::new(Font::fallback()),
        forms: 0,
        report,
        loaded: HashMap::new(),
        shared: fonts,
        oc,
        categories: FastMap::default(),
        marks: M::default(),
    };
    let root = Frame::new(
        Arc::new(content),
        vec![Arc::new(page.resources.clone())],
        GState::new(),
        0,
        (0, 0),
        M::parents_of(page.dict()),
    );
    exec.run(root).await;
    let mut spans = exec.spans;
    for span in &mut spans {
        span.page = page.index;
    }
    // The decoration pass touches only pages that draw horizontal rulings,
    // and each span consults only the rulings inside its vertical band —
    // sorting once keeps a page full of table borders from turning the
    // pass into spans × rulings work.
    let mut horizontals: Vec<&Ruling> = exec
        .rulings
        .iter()
        .filter(|r| r.start.y == r.end.y)
        .collect();
    if !horizontals.is_empty() {
        horizontals.sort_by(|a, b| a.start.y.total_cmp(&b.start.y));
        for span in &mut spans {
            mark_underline_and_strikethrough(span, &horizontals);
        }
    }
    drop(horizontals);
    (spans, exec.rulings, exec.report, exec.marks)
}

/// What a page walk does with marked content, fixed when the walk is
/// compiled: the content-order walk ignores it and pays nothing per
/// operator, the structure-tree walk records each span's sequence.
trait MarkedContent: Default {
    /// Whether `BDC` reads its `/MCID`. A constant, so the operator loop
    /// carries no check at run time.
    const READS_MCID: bool;
    /// The `/StructParents` key a content stream files its sequences under.
    fn parents_of(dict: &Dict) -> Option<u32>;
    /// Notes the sequence the span just emitted came from.
    fn record(&mut self, frame: &Frame);
    /// Drops the notes for the spans a failed stream took back.
    fn truncate(&mut self, len: usize);
}

/// Marked content ignored: the content-order walk.
#[derive(Default)]
struct Ignored;

impl MarkedContent for Ignored {
    const READS_MCID: bool = false;

    fn parents_of(_: &Dict) -> Option<u32> {
        None
    }

    fn record(&mut self, _: &Frame) {}

    fn truncate(&mut self, _: usize) {}
}

/// Each emitted span's marked-content sequence, parallel to the executor's
/// spans: the structure-tree walk.
#[derive(Default)]
struct Recorded {
    ids: Vec<Option<MarkedContentId>>,
}

impl MarkedContent for Recorded {
    const READS_MCID: bool = true;

    fn parents_of(dict: &Dict) -> Option<u32> {
        u32::try_from(dict.get_int("StructParents")?).ok()
    }

    fn record(&mut self, frame: &Frame) {
        self.ids.push(frame.marked_content());
    }

    fn truncate(&mut self, len: usize) {
        self.ids.truncate(len);
    }
}

/// Reorders `spans` into structure-tree order, `marks` being each span's
/// marked-content sequence: a tagged span sorts by its sequence's rank in
/// the tree, an untagged one keeps its place after the last tagged span
/// before it (a running header written first stays first, an artifact
/// written between two paragraphs stays between them). Stable, so spans
/// within one sequence keep content order. `false` when the tree reaches
/// none of the page's marked content, leaving the spans as they were.
async fn structure_order<S: AsyncObjectSource>(
    src: &S,
    tree: &StructureTree,
    page: &Page,
    spans: &mut Vec<TextSpan>,
    marks: &[Option<MarkedContentId>],
) -> bool {
    let ids: Vec<MarkedContentId> = marks.iter().flatten().copied().collect();
    if ids.is_empty() {
        return false;
    }
    let ranks = tree.ranks_with(src, page, &ids).await;
    if ranks.is_empty() {
        return false;
    }
    let mut keyed: Vec<(i64, TextSpan)> = Vec::with_capacity(spans.len());
    let mut current: i64 = -1;
    for (span, mark) in spans.drain(..).zip(marks) {
        if let Some(rank) = mark.and_then(|id| ranks.get(&id)) {
            current = i64::from(*rank);
        }
        keyed.push((current, span));
    }
    keyed.sort_by_key(|(key, _)| *key);
    spans.extend(keyed.into_iter().map(|(_, span)| span));
    true
}

/// How far below the baseline (in fractions of the effective size) an
/// underline may sit, and the slack above it for lines drawn exactly on
/// the baseline.
const UNDERLINE_BELOW: f32 = 0.3;
const UNDERLINE_ABOVE: f32 = 0.05;

/// The x-height band (in fractions of the effective size above the
/// baseline) a strikethrough crosses.
const STRIKETHROUGH_LOW: f32 = 0.15;
const STRIKETHROUGH_HIGH: f32 = 0.6;

/// The fraction of a span's width a ruling must cover to decorate it: a
/// neighbour's underline running past a word boundary is not this span's.
const DECORATED_MIN_OVERLAP: f32 = 0.6;

/// Sets `underline`/`strikethrough` from the page's horizontal rulings
/// (pre-sorted by y): underline when one sits just below the baseline
/// covering most of the span, strikethrough when one crosses the x-height
/// band. Vertical writing is left unmarked — its decorations are vertical
/// lines beside the text, which are indistinguishable from column rules
/// here.
fn mark_underline_and_strikethrough(span: &mut TextSpan, horizontals: &[&Ruling]) {
    if span.vertical || span.size <= 0.0 {
        return;
    }
    let width = span.bbox.x1 - span.bbox.x0;
    if width <= 0.0 {
        return;
    }
    let low = span.y - UNDERLINE_BELOW * span.size;
    let high = span.y + STRIKETHROUGH_HIGH * span.size;
    let first = horizontals.partition_point(|r| r.start.y < low);
    for r in &horizontals[first..] {
        if r.start.y > high {
            break;
        }
        let overlap = r.end.x.min(span.bbox.x1) - r.start.x.max(span.bbox.x0);
        if overlap < DECORATED_MIN_OVERLAP * width {
            continue;
        }
        let above = r.start.y - span.y;
        if above <= UNDERLINE_ABOVE * span.size {
            span.underline = true;
        }
        if above >= STRIKETHROUGH_LOW * span.size {
            span.strikethrough = true;
        }
    }
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
    /// `Tr` (ISO 32000-1 Table 106); modes 3 and 7 paint nothing.
    render_mode: i32,
    /// Fill color as RGB; `None` inside a pattern fill.
    fill_color: Option<(f32, f32, f32)>,
    /// `w` and ExtGState `/LW`; scales rulings' stroke width.
    line_width: f32,
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
            render_mode: 0,
            fill_color: Some((0.0, 0.0, 0.0)),
            line_width: 1.0,
        }
    }
}

/// Reads color components by count — 1 gray, 3 RGB, 4 CMYK, clamped to
/// `[0, 1]` — the approximation span colors carry for spaces whose
/// transform extraction does not run. Any other count is no color.
fn components_color(comps: &[f32]) -> Option<(f32, f32, f32)> {
    let c = |v: f32| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    match comps {
        [v] => Some((c(*v), c(*v), c(*v))),
        [r, g, b] => Some((c(*r), c(*g), c(*b))),
        [cy, m, y, k] => Some((
            (1.0 - c(*cy)) * (1.0 - c(*k)),
            (1.0 - c(*m)) * (1.0 - c(*k)),
            (1.0 - c(*y)) * (1.0 - c(*k)),
        )),
        _ => None,
    }
}

/// True when every matrix component is finite.
fn finite(m: &Matrix) -> bool {
    [m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite())
}

/// Isotropic scale factor of `m`: `sqrt(|det|)`, 1.0 when degenerate — the
/// same rule rendering uses to carry a line width into device space.
fn ctm_scale(m: &Matrix) -> f32 {
    let det = (m.a * m.d - m.b * m.c).abs();
    if det.is_finite() && det > 0.0 {
        return det.sqrt();
    }
    1.0
}

/// Classifies one device-space segment: `Some` when it is axis-aligned
/// within [`RULING_AXIS_EPSILON`] and at least [`RULING_MIN_LENGTH`] long.
fn ruling_from_segment(a: Point, b: Point, width: f32) -> Option<Ruling> {
    if [a.x, a.y, b.x, b.y, width].iter().any(|v| !v.is_finite()) {
        return None;
    }
    let dx = (b.x - a.x).abs();
    let dy = (b.y - a.y).abs();
    if dy <= RULING_AXIS_EPSILON && dx >= RULING_MIN_LENGTH {
        let y = (a.y + b.y) / 2.0;
        return Some(Ruling {
            start: Point::new(a.x.min(b.x), y),
            end: Point::new(a.x.max(b.x), y),
            width,
        });
    }
    if dx <= RULING_AXIS_EPSILON && dy >= RULING_MIN_LENGTH {
        let x = (a.x + b.x) / 2.0;
        return Some(Ruling {
            start: Point::new(x, a.y.min(b.y)),
            end: Point::new(x, a.y.max(b.y)),
            width,
        });
    }
    None
}

/// The centerline of a thin filled bar: a closed 4-vertex subpath in device
/// space whose bounding box has a thin dimension at most
/// [`RULING_MAX_FILL_THICKNESS`] and a long dimension at least
/// [`RULING_MIN_LENGTH`]. The box being thin is the whole test: a rectangle
/// qualifies, and so does the mitered bar some producers draw table borders
/// as — axis-aligned long edges, beveled ends — while a diagonal sliver's
/// box is fat in both dimensions and never qualifies. Width is 0.0 — a fill
/// has no stroke width.
fn filled_rect_ruling(device: &[Point]) -> Option<Ruling> {
    let corners = match device {
        [a, b, c, d] => [*a, *b, *c, *d],
        [a, b, c, d, e]
            if (e.x - a.x).abs() <= RULING_AXIS_EPSILON
                && (e.y - a.y).abs() <= RULING_AXIS_EPSILON =>
        {
            [*a, *b, *c, *d]
        }
        _ => return None,
    };
    if corners.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
        return None;
    }
    let x0 = corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let x1 = corners
        .iter()
        .map(|p| p.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let y0 = corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let y1 = corners
        .iter()
        .map(|p| p.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let w = x1 - x0;
    let h = y1 - y0;
    if w.min(h) > RULING_MAX_FILL_THICKNESS || w.max(h) < RULING_MIN_LENGTH {
        return None;
    }
    if h <= w {
        let y = (y0 + y1) / 2.0;
        return Some(Ruling {
            start: Point::new(x0, y),
            end: Point::new(x1, y),
            width: 0.0,
        });
    }
    let x = (x0 + x1) / 2.0;
    Some(Ruling {
        start: Point::new(x, y0),
        end: Point::new(x, y1),
        width: 0.0,
    })
}

/// One subpath under construction, in the frame's untransformed user space.
///
/// A curve operator poisons it — glyph outlines and diagrams are not
/// rulings — but still advances the endpoint, so a following `l` extends
/// the poisoned subpath instead of corrupting the next one.
struct Subpath {
    points: Vec<Point>,
    closed: bool,
    poisoned: bool,
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
    /// The stream's decoded bytes; operators are pull-parsed from it one at
    /// a time, never materialized as a vector. Shared rather than owned so
    /// a handle can be held while the frame stack is pushed onto — cloned
    /// once per visit to the frame, never per operator.
    content: Arc<Vec<u8>>,
    /// Resource dictionaries, innermost first. Owned, because a form's own
    /// `/Resources` is read out of its stream dictionary and so outlives nothing
    /// already on the stack.
    chain: Vec<Arc<Dict>>,
    /// Byte offset of the next operator: the pull parser's whole state at
    /// an operator boundary, so a suspended frame resumes from it exactly.
    pos: usize,
    /// Lengths of the executor's spans and rulings when this frame was
    /// created. A stream that stops parsing mid-way contributes nothing —
    /// exactly as it contributed nothing when the whole stream was parsed
    /// up front — so its error truncates both back to these marks.
    spans_mark: usize,
    rulings_mark: usize,
    /// Form-XObject nesting depth, checked against `MAX_FORM_DEPTH`.
    depth: usize,
    gs: GState,
    /// The `q`/`Q` stack, per operator stream.
    saved: Vec<GState>,
    tm: Matrix,
    tlm: Matrix,
    /// Path accumulation for rulings, per operator stream like `tm`/`tlm`:
    /// the last element is the active subpath. Not part of [`GState`] —
    /// `q`/`Q` do not save or restore the path.
    subpaths: Vec<Subpath>,
    /// Loaded fonts, per operator stream: every form invocation starts with an
    /// empty cache, as it did when each invocation was its own `run` call.
    fonts: HashMap<String, Arc<Font>>,
    /// The marked-content stack: one entry per open `BMC`/`BDC`. Per frame
    /// like `tm`/`tlm` — `BMC`/`EMC` nesting is not `q`/`Q` scoped and never
    /// crosses a stream boundary. A stray `EMC` pops nothing.
    marks: Vec<Mark>,
    /// The `/StructParents` key of this content stream: the page's, or a
    /// form XObject's own when it declares one, else its caller's.
    parents: Option<u32>,
}

/// One open marked-content sequence: whether a `BDC /OC` span the
/// optional-content configuration hides, and its `/MCID` when the walk is
/// tracking marked content for the structure tree.
#[derive(Clone, Copy)]
struct Mark {
    hidden: bool,
    mcid: Option<u32>,
}

impl Frame {
    fn new(
        content: Arc<Vec<u8>>,
        chain: Vec<Arc<Dict>>,
        gs: GState,
        depth: usize,
        (spans_mark, rulings_mark): (usize, usize),
        parents: Option<u32>,
    ) -> Frame {
        Frame {
            content,
            chain,
            pos: 0,
            spans_mark,
            rulings_mark,
            depth,
            gs,
            saved: Vec::new(),
            tm: Matrix::identity(),
            tlm: Matrix::identity(),
            subpaths: Vec::new(),
            fonts: HashMap::new(),
            marks: Vec::new(),
            parents,
        }
    }

    /// Whether the frame is inside a marked-content span the
    /// optional-content configuration hides: state still executes, but the
    /// span's text and rulings are excluded from the result.
    fn suppressed(&self) -> bool {
        self.marks.iter().any(|mark| mark.hidden)
    }

    /// The marked-content sequence the frame is inside: the innermost open
    /// mark carrying an `/MCID`, under this stream's `/StructParents` key.
    /// `None` outside any sequence, or when the stream has no key to file
    /// the sequence under.
    fn marked_content(&self) -> Option<MarkedContentId> {
        let parents = self.parents?;
        let mcid = self.marks.iter().rev().find_map(|mark| mark.mcid)?;
        Some(MarkedContentId { parents, mcid })
    }

    fn move_to(&mut self, x: f32, y: f32) {
        self.subpaths.push(Subpath {
            points: vec![Point::new(x, y)],
            closed: false,
            poisoned: false,
        });
    }

    /// Extends the active subpath by one segment. Appending to a closed
    /// subpath begins a new one at the closed subpath's starting point
    /// (ISO 32000-1 §8.5.2.1); with no current point at all the operator
    /// is ignored.
    fn segment_to(&mut self, x: f32, y: f32, poisons: bool) {
        let Some(active) = self.subpaths.last_mut() else {
            return;
        };
        if active.closed {
            let start = active.points[0];
            self.subpaths.push(Subpath {
                points: vec![start, Point::new(x, y)],
                closed: false,
                poisoned: poisons,
            });
            return;
        }
        active.points.push(Point::new(x, y));
        if poisons {
            active.poisoned = true;
        }
    }

    fn close_subpath(&mut self) {
        if let Some(active) = self.subpaths.last_mut() {
            active.closed = true;
        }
    }

    /// Appends `re` as the closed subpath rendering's path builder makes of
    /// it: the `(x, y)` corner, then the three others in `re`'s own order.
    /// Raw corners even for negative `w`/`h` — the current point a
    /// follow-on segment continues from is `(x, y)` — normalization happens
    /// when the committed rectangle's bounding box is measured.
    fn rect_subpath(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.subpaths.push(Subpath {
            points: vec![
                Point::new(x, y),
                Point::new(x + w, y),
                Point::new(x + w, y + h),
                Point::new(x, y + h),
            ],
            closed: true,
            poisoned: false,
        });
    }
}

struct Executor<'a, S, M> {
    src: &'a S,
    spans: Vec<TextSpan>,
    rulings: Vec<Ruling>,
    fallback: Arc<Font>,
    /// Form-XObject invocations so far, checked against
    /// `MAX_FORM_INVOCATIONS`.
    forms: usize,
    /// What could not be read; carried out alongside the spans.
    report: ExtractReport,
    /// Fonts loaded during this page walk, keyed by their dictionary's
    /// object reference — shared across every frame the walk pushes, so a
    /// form invoked many times loads its fonts once. Never keyed by name:
    /// that binding is per resource scope and stays in [`Frame::fonts`].
    loaded: HashMap<ObjRef, Arc<Font>>,
    /// Fonts carried across page walks, when the caller extracts a whole
    /// document and passes one [`FontCache`] to every page.
    shared: Option<&'a FontCache>,
    /// The document's optional-content visibility; `None` extracts every
    /// layer.
    oc: Option<&'a OcState>,
    /// Resolved resource-category dictionaries, keyed by the resource
    /// dictionary's allocation address plus a category slot. Resolving a
    /// category hands out a deep clone of the whole dictionary, and `gs`
    /// and `Do` used to pay that per operator — a third of a form-heavy
    /// corpus extraction pass. `None` remembers a category the dictionary
    /// does not carry (or that is not a dictionary). The held [`Arc`] keeps
    /// the resource dictionary's allocation alive, so the address cannot be
    /// reused while its entry exists. See [`MAX_CATEGORY_CACHE`].
    categories: FastMap<(usize, u8), ResolvedCategory>,
    /// The walk's marked-content strategy (see [`MarkedContent`]).
    marks: M,
}

/// One memoized resource category: the resource dictionary whose allocation
/// the entry pins (its address is the cache key) and its resolved category
/// dictionary, or `None` for a remembered absence.
type ResolvedCategory = (Arc<Dict>, Option<Arc<Dict>>);

/// Upper bound on memoized (resource dictionary, category) pairs per page
/// walk; past it, lookups resolve uncached, so a hostile file minting
/// resource dictionaries per form invocation caps the memo's memory.
const MAX_CATEGORY_CACHE: usize = 4096;

/// The memo slot for a resource category name, [`None`] for a category no
/// caller looks up hot (left uncached rather than given an open-ended key).
fn category_slot(category: &str) -> Option<u8> {
    match category {
        "ExtGState" => Some(0),
        "XObject" => Some(1),
        _ => None,
    }
}

impl<S: AsyncObjectSource, M: MarkedContent> Executor<'_, S, M> {
    /// Looks up `/category/name` in the resource chain, innermost dictionary
    /// first (ISO 32000 §7.8.3).
    ///
    /// A nested form's own `/Resources` shadows its caller's for the names it
    /// defines and falls through for the ones it does not. This mirrors the
    /// renderer's `find_res`; the two crates must agree on which resource a
    /// name refers to, or the same file extracts different text than it
    /// paints.
    async fn find_res(
        &mut self,
        chain: &[Arc<Dict>],
        category: &str,
        name: &str,
    ) -> Option<Object> {
        let slot = category_slot(category);
        for res in chain {
            let key = (Arc::as_ptr(res) as usize, slot.unwrap_or(0));
            let remembered = slot.and_then(|_| {
                self.categories
                    .get(&key)
                    .map(|(_, category)| category.clone())
            });
            let resolved = match remembered {
                Some(dict) => dict,
                None => {
                    let dict = match res.get(category) {
                        Some(cat) => match self.src.resolve(cat).await {
                            Ok(Object::Dict(d)) => Some(Arc::new(d)),
                            _ => None,
                        },
                        None => None,
                    };
                    if slot.is_some() && self.categories.len() < MAX_CATEGORY_CACHE {
                        self.categories.insert(key, (Arc::clone(res), dict.clone()));
                    }
                    dict
                }
            };
            let Some(dict) = resolved else {
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
        &mut self,
        chain: &[Arc<Dict>],
        name: &str,
        cache: &mut HashMap<String, Arc<Font>>,
    ) -> Arc<Font> {
        if let Some(f) = cache.get(name) {
            return f.clone();
        }
        let loaded = self.load_font(chain, name).await;
        if !loaded.simple && !loaded.encoding_known {
            self.report
                .record(SkippedTextKind::FontEncoding, SkipCause::Missing);
        }
        cache.insert(name.to_string(), loaded.clone());
        loaded
    }

    /// Resolves `name` through the chain with [`Self::find_res`]'s exact
    /// semantics — innermost scope first, a name whose value will not resolve
    /// falls through to the outer scopes, the first value that resolves wins
    /// whatever it turns out to be — and loads the font it lands on.
    ///
    /// A value held as an indirect reference is answered from the caches
    /// before it is even resolved: within one document a reference resolves
    /// to the same dictionary every time, so an already-loaded font is the
    /// same font. Anything else — a direct dictionary, a value that is no
    /// dictionary at all (the fallback), an exhausted chain (also the
    /// fallback) — is loaded per use, cached only under its name in the
    /// calling frame.
    async fn load_font(&mut self, chain: &[Arc<Dict>], name: &str) -> Arc<Font> {
        for res in chain {
            let Some(cat) = res.get("Font") else {
                continue;
            };
            let Ok(Object::Dict(dict)) = self.src.resolve(cat).await else {
                continue;
            };
            let Some(value) = dict.get(name) else {
                continue;
            };
            let key = match value {
                Object::Ref(r) => Some(*r),
                _ => None,
            };
            if let Some(f) = key.and_then(|r| self.hit(r)) {
                return f;
            }
            let Ok(obj) = self.src.resolve(value).await else {
                continue;
            };
            let Some(font_dict) = obj.as_dict() else {
                // The name resolved to something that is not a dictionary:
                // the fallback font keeps the text extractable rather than
                // failing the page.
                return self.fallback.clone();
            };
            let loaded = Arc::new(Font::load(self.src, font_dict).await);
            return match key {
                Some(r) => self.remember(r, loaded),
                None => loaded,
            };
        }
        self.fallback.clone()
    }

    /// An already-loaded font for the dictionary `r` refers to, if any walk
    /// of this document has loaded it.
    fn hit(&mut self, r: ObjRef) -> Option<Arc<Font>> {
        if let Some(f) = self.loaded.get(&r) {
            return Some(f.clone());
        }
        let f = self.shared?.get(r)?;
        self.loaded.insert(r, f.clone());
        Some(f)
    }

    /// Records a freshly loaded font under its dictionary's reference, in
    /// this walk's cache and in the document-wide one when present.
    fn remember(&mut self, r: ObjRef, font: Arc<Font>) -> Arc<Font> {
        let font = match self.shared {
            Some(shared) => shared.insert(r, font),
            None => font,
        };
        self.loaded.insert(r, font.clone());
        font
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
            let content = Arc::clone(&frame.content);
            let mut ops = ContentOps::at(&content, frame.pos);
            loop {
                let op = match ops.next_op() {
                    Ok(Some((op, _))) => op,
                    Ok(None) => break,
                    Err(_) => {
                        // The stream stops parsing mid-way: it contributes
                        // nothing, exactly as it contributed nothing when
                        // the whole stream was parsed up front.
                        self.spans.truncate(frame.spans_mark);
                        self.marks.truncate(frame.spans_mark);
                        self.rulings.truncate(frame.rulings_mark);
                        let kind = if frame.depth == 0 {
                            SkippedTextKind::PageContents
                        } else {
                            SkippedTextKind::Form
                        };
                        self.report.record(kind, SkipCause::Parse);
                        continue 'frames;
                    }
                };
                match &op {
                    Op::SetFont(name, size) => {
                        let loaded = self.font(&frame.chain, &name.0, &mut frame.fonts).await;
                        frame.gs.font = Some(loaded);
                        frame.gs.font_name = name.0.clone();
                        frame.gs.size = *size;
                    }
                    Op::SetExtGState(name) => {
                        if let Some(lw) = self.ext_gstate_line_width(&frame.chain, &name.0).await {
                            frame.gs.line_width = lw;
                        }
                    }
                    Op::XObject(name) => {
                        // Inside a hidden span the whole invocation is part
                        // of the span: never entered, never reported.
                        if frame.suppressed() {
                            continue;
                        }
                        let entered = self
                            .form_frame(
                                &name.0,
                                &frame.chain,
                                &frame.gs,
                                frame.depth,
                                frame.parents,
                            )
                            .await;
                        if let Some(child) = entered {
                            // The caller goes back underneath its form: the form
                            // runs to completion, then the caller resumes at the
                            // operator after its `Do`. That is the depth-first
                            // order the recursive version emitted.
                            frame.pos = ops.pos();
                            frames.push(frame);
                            frames.push(child);
                            continue 'frames;
                        }
                    }
                    Op::BeginMarkedContentProps(tag, props) => {
                        let hidden = self.marked_hidden(tag, props, &frame.chain).await;
                        if hidden {
                            self.report.hidden += 1;
                        }
                        let mcid = if M::READS_MCID {
                            self.marked_mcid(props, &frame.chain).await
                        } else {
                            None
                        };
                        frame.marks.push(Mark { hidden, mcid });
                    }
                    op => self.step(&mut frame, op),
                }
            }
        }
    }

    /// The `/LW` entry of the named `/ExtGState` resource (ISO 32000-1
    /// Table 58) — the one ExtGState parameter ruling extraction reads.
    /// Negative values are ignored, matching the renderer; non-finite ones
    /// too, because an infinite line width would otherwise silently drop
    /// every later stroked ruling at the segment gate.
    async fn ext_gstate_line_width(&mut self, chain: &[Arc<Dict>], name: &str) -> Option<f32> {
        let resolved = self.find_res(chain, "ExtGState", name).await?;
        let dict = resolved.as_dict()?;
        let lw = self.src.resolve(dict.get("LW")?).await.ok()?.as_f64()? as f32;
        (lw.is_finite() && lw >= 0.0).then_some(lw)
    }

    /// Applies one operator that needs no I/O — everything except `Tf`,
    /// `gs`, and `Do`. `q`/`Q` and `cm` maintain the CTM; text operators
    /// maintain Tm/Tlm; shown strings become spans; path operators feed the
    /// frame's subpaths and paint operators commit them as rulings.
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
            Op::SetTextRender(mode) => frame.gs.render_mode = *mode,
            Op::SetFillGray(v) => frame.gs.fill_color = components_color(&[*v]),
            Op::SetFillRGB(r, g, b) => frame.gs.fill_color = components_color(&[*r, *g, *b]),
            Op::SetFillCMYK(c, m, y, k) => {
                frame.gs.fill_color = components_color(&[*c, *m, *y, *k])
            }
            // Selecting a fill space resets the fill color to the space's
            // initial color (ISO 32000-1 §8.6.8): black everywhere but
            // Pattern, which has no single color.
            Op::SetFillColorSpace(name) => {
                frame.gs.fill_color = (name.0 != "Pattern").then_some((0.0, 0.0, 0.0));
            }
            Op::SetFillColor(comps) => frame.gs.fill_color = components_color(comps),
            Op::SetFillColorN(comps, pattern) => {
                frame.gs.fill_color = if pattern.is_some() {
                    None
                } else {
                    components_color(comps)
                };
            }
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
                // In vertical writing the TJ offset moves ty, and Tz does
                // not apply to vertical displacements (ISO 32000-1 §9.4.4).
                let vertical = frame.gs.font.as_ref().is_some_and(|f| f.vertical);
                for item in items {
                    match item {
                        TextItem::Str(s) => self.emit(frame, s),
                        TextItem::Offset(n) => {
                            let (tx, ty) = if vertical {
                                (0.0, -n / 1000.0 * frame.gs.size)
                            } else {
                                (-n / 1000.0 * frame.gs.size * frame.gs.horiz_scale, 0.0)
                            };
                            if tx.is_finite() && ty.is_finite() {
                                frame.tm = Matrix::translate(tx, ty).concat(frame.tm);
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
            Op::SetLineWidth(w) => {
                if w.is_finite() && *w >= 0.0 {
                    frame.gs.line_width = *w;
                }
            }
            Op::MoveTo(x, y) => frame.move_to(*x, *y),
            Op::LineTo(x, y) => frame.segment_to(*x, *y, false),
            Op::CurveTo(_, _, _, _, x, y) | Op::CurveToV(_, _, x, y) | Op::CurveToY(_, _, x, y) => {
                frame.segment_to(*x, *y, true)
            }
            Op::ClosePath => frame.close_subpath(),
            Op::Rect(x, y, w, h) => frame.rect_subpath(*x, *y, *w, *h),
            Op::Stroke => self.commit_rulings(frame, true, false),
            Op::CloseStroke => {
                frame.close_subpath();
                self.commit_rulings(frame, true, false);
            }
            Op::Fill | Op::FillEvenOdd => self.commit_rulings(frame, false, true),
            Op::FillStroke | Op::FillStrokeEvenOdd => self.commit_rulings(frame, true, true),
            Op::CloseFillStroke | Op::CloseFillStrokeEvenOdd => {
                frame.close_subpath();
                self.commit_rulings(frame, true, true);
            }
            // `W`/`W*` never commit by themselves: the paint operator that
            // must follow them does, and after a clip that operator is `n`.
            Op::EndPath => frame.subpaths.clear(),
            // Marked content: every open is pushed (hidden or not) so `EMC`
            // stays balanced; `BDC` needs I/O and is handled in `run`.
            Op::BeginMarkedContent(_) => frame.marks.push(Mark {
                hidden: false,
                mcid: None,
            }),
            Op::EndMarkedContent => {
                frame.marks.pop();
            }
            // Text render mode 3 (invisible) is still extracted — the
            // document shows that text, a viewer just paints it blank.
            // Optional content is the opposite species: the document
            // declares the layer off, so a hidden span IS skipped (see
            // `emit`). `Tr` and everything else is a no-op here.
            _ => {}
        }
    }

    /// Whether a `BDC` opens a span the optional-content configuration
    /// hides: only `/OC` tags gate anything, and with no configuration (or
    /// anything unresolvable) every span is visible.
    async fn marked_hidden(&self, tag: &Name, props: &Object, chain: &[Arc<Dict>]) -> bool {
        let Some(oc) = self.oc else {
            return false;
        };
        if tag.0 != "OC" {
            return false;
        }
        !oc.props_visible_with(self.src, chain, props).await
    }

    /// The `/MCID` a `BDC` opens: read from an inline property dictionary,
    /// or from the named one in the resource chain's `/Properties`.
    async fn marked_mcid(&mut self, props: &Object, chain: &[Arc<Dict>]) -> Option<u32> {
        let named;
        let dict = match props {
            Object::Dict(dict) => dict,
            Object::Name(name) => {
                named = self.find_res(chain, "Properties", &name.0).await?;
                named.as_dict()?
            }
            _ => return None,
        };
        u32::try_from(dict.get_int("MCID")?).ok()
    }

    /// Commits the accumulated path on a painting operator and clears it.
    /// Stroked subpaths yield one ruling per axis-aligned segment at the
    /// device-space line width; filled subpaths yield the centerline of a
    /// thin axis-aligned rectangle. Poisoned subpaths yield nothing.
    fn commit_rulings(&mut self, frame: &mut Frame, stroke: bool, fill: bool) {
        // A hidden span's lines are configured away with its text; the
        // path still clears, exactly as a paint operator leaves it.
        if frame.suppressed() {
            frame.subpaths.clear();
            return;
        }
        let ctm = frame.gs.ctm;
        let width = frame.gs.line_width * ctm_scale(&ctm);
        for sub in frame.subpaths.drain(..) {
            if sub.poisoned {
                continue;
            }
            let device: Vec<Point> = sub.points.iter().map(|p| ctm.apply(*p)).collect();
            if stroke {
                let segments = device.windows(2).map(|pair| (pair[0], pair[1]));
                // A closed 2-point subpath draws one doubled edge, not two.
                let closing =
                    (sub.closed && device.len() > 2).then(|| (device[device.len() - 1], device[0]));
                for (a, b) in segments.chain(closing) {
                    if let Some(ruling) = ruling_from_segment(a, b, width) {
                        self.rulings.push(ruling);
                    }
                }
            }
            if fill {
                if let Some(ruling) = filled_rect_ruling(&device) {
                    self.rulings.push(ruling);
                }
            }
        }
    }

    /// Shows one string, appending the span it produces (if any) to the
    /// page. Inside a hidden optional-content span the advance still runs —
    /// `show` moves the text matrix either way — but the text is excluded.
    fn emit(&mut self, frame: &mut Frame, bytes: &[u8]) {
        let suppressed = frame.suppressed();
        if let Some(span) = self.show(&frame.gs, &mut frame.tm, bytes) {
            if !suppressed {
                self.spans.push(span);
                self.marks.record(frame);
            }
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
    fn show(&self, gs: &GState, tm: &mut Matrix, bytes: &[u8]) -> Option<TextSpan> {
        let font: &Font = gs.font.as_deref().unwrap_or(&self.fallback);
        let start = tm.concat(gs.ctm);
        let origin = start.apply(Point { x: 0.0, y: gs.rise });
        // Device-space font size: the length of the text-space vertical
        // unit vector scaled by Tfs under Tm·CTM.
        let size = gs.size * (start.c * start.c + start.d * start.d).sqrt();
        // One byte per code is the floor on the decoded length, so this
        // reservation removes the per-glyph regrowth of typical text.
        let mut text = String::with_capacity(bytes.len());
        for cc in font.codes_in(bytes) {
            font.decode_into(cc, &mut text);
            let word = if font.is_space(cc) {
                gs.word_spacing
            } else {
                0.0
            };
            // Vertical writing advances ty by w1 (negative for downward),
            // with Tz not applied to vertical displacements (ISO 32000-1
            // §9.4.4); horizontal advances tx as before.
            let adv = if font.vertical {
                font.vwidth(cc) / 1000.0 * gs.size + gs.char_spacing + word
            } else {
                (font.width(cc) / 1000.0 * gs.size + gs.char_spacing + word) * gs.horiz_scale
            };
            if adv.is_finite() {
                let (tx, ty) = if font.vertical {
                    (0.0, adv)
                } else {
                    (adv, 0.0)
                };
                *tm = Matrix::translate(tx, ty).concat(*tm);
            }
        }
        let end = tm.concat(gs.ctm).apply(Point { x: 0.0, y: gs.rise });
        let size = if size.is_finite() { size } else { 0.0 };
        let bbox = if font.vertical {
            Rect {
                x0: origin.x - size / 2.0,
                y0: origin.y.min(end.y),
                x1: origin.x + size / 2.0,
                y1: origin.y.max(end.y),
            }
        } else {
            Rect {
                x0: origin.x.min(end.x),
                y0: origin.y + font.descent / 1000.0 * size,
                x1: origin.x.max(end.x),
                y1: origin.y + font.ascent / 1000.0 * size,
            }
        };
        (!text.is_empty() && origin.x.is_finite() && origin.y.is_finite()).then(|| TextSpan {
            text,
            x: origin.x,
            y: origin.y,
            end_x: end.x,
            size,
            bbox,
            font: gs.font_name.clone(),
            font_name: font.base_name.clone(),
            page: 0,
            bold: font.bold,
            italic: font.italic,
            monospace: font.monospace,
            serif: font.serif,
            rise: gs.rise,
            vertical: font.vertical,
            invisible: matches!(gs.render_mode, 3 | 7),
            color: gs.fill_color,
            underline: false,
            strikethrough: false,
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
        parents: Option<u32>,
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
        // A form with a hidden `/OC` entry is configured away with its
        // whole subtree: counted on the dedicated counter, never a skip.
        if let (Some(oc), Some(gate)) = (self.oc, stream.dict.get("OC")) {
            if !oc.visible_with(self.src, gate).await {
                self.report.hidden += 1;
                return None;
            }
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
        // A form declaring its own `/StructParents` files its marked content
        // under that key (ISO 32000-1 §14.7.4.4); one without inherits its
        // caller's.
        Some(Frame::new(
            Arc::new(data),
            inner_chain,
            inner,
            depth + 1,
            (self.spans.len(), self.rulings.len()),
            M::parents_of(&stream.dict).or(parents),
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{block_on, Document, Immediate};
    use pdfboss_testkit::doc_with_graphics;

    /// The synchronous spans accessor. Production has no use for one — the public
    /// entry points in `lib.rs` wrap [`page_spans_and_rulings_with`] themselves —
    /// but it is the same `block_on` over `Immediate`, so every test below still
    /// asserts on exactly what a synchronous caller receives. The report is
    /// asserted complete: no test here expects to lose content.
    fn page_spans(doc: &Document, page: &Page) -> Vec<TextSpan> {
        let (spans, _, report) = extract_all(doc, page);
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        spans
    }

    /// The synchronous rulings accessor, the twin of [`page_spans`].
    fn page_rulings(doc: &Document, page: &Page) -> Vec<Ruling> {
        let (_, rulings, report) = extract_all(doc, page);
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        rulings
    }

    /// One walk with the document's own optional-content configuration —
    /// exactly what the `lib.rs` document-level entries drive.
    fn extract_all(doc: &Document, page: &Page) -> (Vec<TextSpan>, Vec<Ruling>, ExtractReport) {
        let oc = doc.oc_state();
        block_on(page_spans_and_rulings_with(
            Immediate(doc),
            page,
            None,
            oc.as_ref(),
            None,
            ReadingOrder::Content,
        ))
    }

    /// One page over two optional content groups: object 8 stays on,
    /// object 9 is off in the default configuration, reachable from
    /// content as `/Properties` entries `/V` and `/H`; `/Fx` is a form
    /// gated off by its own `/OC` entry.
    fn oc_doc(content: &[u8]) -> Document {
        use pdfboss_testkit::PdfBuilder;
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /OCProperties \
             << /OCGs [8 0 R 9 0 R] /D << /OFF [9 0 R] >> >> >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> \
             /Properties << /V 8 0 R /H 9 0 R >> \
             /XObject << /Fx 6 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", content);
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] /OC 9 0 R",
            b"BT /F1 12 Tf 72 600 Td (formtext) Tj ET",
        );
        b.object(8, "<< /Type /OCG /Name (shown) >>");
        b.object(9, "<< /Type /OCG /Name (hidden) >>");
        Document::load(b.build(1)).expect("load")
    }

    /// A hidden layer's text is excluded and counted once per span, while
    /// its advances still run: the visible text that follows starts where
    /// the hidden run ended, and the walk is still complete.
    #[test]
    fn hidden_layer_text_is_excluded_but_still_advances() {
        let doc = oc_doc(
            b"BT /F1 12 Tf 72 720 Td /OC /H BDC (wide hidden run) Tj EMC (kept) Tj \
              /OC /V BDC ( on) Tj EMC ET",
        );
        let page = doc.page(0).unwrap();
        let (spans, _, report) = extract_all(&doc, &page);
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["kept", " on"]);
        assert!(
            spans[0].x > 100.0,
            "the hidden run must still advance: x = {}",
            spans[0].x
        );
        assert_eq!(report.hidden, 1);
        assert!(report.is_complete(), "hidden is not a skip: {report:?}");
    }

    /// A form whose own `/OC` entry is off contributes nothing — no spans,
    /// no skip entry, one count — and rulings drawn in a hidden span are
    /// excluded with the text.
    #[test]
    fn hidden_forms_and_rulings_are_excluded() {
        let doc = oc_doc(b"/Fx Do /OC /H BDC 72 700 m 272 700 l S EMC 72 650 m 272 650 l S");
        let page = doc.page(0).unwrap();
        let (spans, rulings, report) = extract_all(&doc, &page);
        assert_eq!(spans, vec![], "the gated form must not run");
        assert_eq!(rulings.len(), 1, "only the visible line survives");
        assert!((rulings[0].start.y - 650.0).abs() < 1e-3);
        assert_eq!(report.hidden, 2, "one form, one span");
        assert!(report.is_complete());
    }

    /// `3 Tr` text is a viewer-invisible layer the document still shows —
    /// searchable-scan OCR — and stays extracted; an off optional-content
    /// layer is declared off by the document itself and is excluded. The
    /// two must not be conflated.
    #[test]
    fn invisible_render_mode_survives_where_hidden_layers_do_not() {
        let doc = oc_doc(
            b"BT /F1 12 Tf 3 Tr 72 720 Td (ocr) Tj ET \
              /OC /H BDC BT /F1 12 Tf 72 700 Td (gone) Tj ET EMC",
        );
        let page = doc.page(0).unwrap();
        let (spans, _, report) = extract_all(&doc, &page);
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["ocr"]);
        assert_eq!(report.hidden, 1);
    }

    /// Without `/OCProperties` there is no configuration to be off in:
    /// every `/OC` span extracts and nothing is counted.
    #[test]
    fn absent_configuration_extracts_every_layer() {
        use pdfboss_testkit::PdfBuilder;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> \
             /Properties << /H 8 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf 72 720 Td /OC /H BDC (loose) Tj EMC ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.object(8, "<< /Type /OCG /Name (loose) >>");
        let doc = Document::load(b.build(1)).expect("load");
        let page = doc.page(0).unwrap();
        let (spans, _, report) = extract_all(&doc, &page);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "loose");
        assert_eq!(report.hidden, 0);
    }

    /// Raw spans of a one-page document with `content` as its raw content
    /// stream (12pt /F1 with default widths of 500).
    fn spans_of(content: &str) -> Vec<TextSpan> {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        page_spans(&doc, &page)
    }

    /// Raw rulings of a one-page document with `content` as its raw content
    /// stream.
    fn rulings_of(content: &str) -> Vec<Ruling> {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        page_rulings(&doc, &page)
    }

    #[track_caller]
    fn assert_ruling(r: &Ruling, x0: f32, y0: f32, x1: f32, y1: f32) {
        let close = (r.start.x - x0).abs() < 1e-3
            && (r.start.y - y0).abs() < 1e-3
            && (r.end.x - x1).abs() < 1e-3
            && (r.end.y - y1).abs() < 1e-3;
        assert!(close, "{r:?} is not ({x0},{y0})-({x1},{y1})");
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

    /// `T*` moves to the next line by translating Tlm by `(0, -leading)`,
    /// the same geometry `'` relies on to start its shown line.
    #[test]
    fn t_star_advances_tlm_by_leading() {
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
    fn empty_content_yields_no_spans() {
        assert!(spans_of("").is_empty());
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
        let (spans, _, report) = block_on(page_spans_and_rulings_with(
            Immediate(&doc),
            &page,
            None,
            None,
            None,
            ReadingOrder::Content,
        ));
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
    /// glyph cache came to block a spawnable future. [`FontCache`] is on the
    /// list because one instance serves every worker of a parallel page walk.
    #[test]
    fn loaded_fonts_are_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Font>();
        assert_send_sync::<Arc<Font>>();
        assert_send_sync::<GState>();
        assert_send_sync::<FontCache>();
    }

    /// `/F1` in a form's own resources and `/F1` in the page resources are
    /// different fonts: the name→font binding is resource-scoped (ISO 32000
    /// §7.8.3). The loaded-font cache is keyed by the font dictionary's
    /// object reference, never by name — a cache keyed by name would hand
    /// the form the page's font and fail this test.
    #[test]
    fn same_name_binds_a_different_font_per_resource_scope() {
        use pdfboss_testkit::PdfBuilder;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (aa) Tj ET /Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding /FirstChar 97 /LastChar 97 /Widths [500] >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >>",
            b"BT /F1 12 Tf 72 700 Td (aa) Tj ET",
        );
        b.object(
            7,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding /FirstChar 97 /LastChar 97 /Widths [1000] >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = page_spans(&doc, &page);
        assert_eq!(spans.len(), 2);
        let advance = |s: &TextSpan| s.end_x - s.x;
        assert!(
            (advance(&spans[0]) - 12.0).abs() < 1e-3,
            "page scope must use the 500-width font: {}",
            advance(&spans[0])
        );
        assert!(
            (advance(&spans[1]) - 24.0).abs() < 1e-3,
            "form scope must use the 1000-width font: {}",
            advance(&spans[1])
        );
    }

    /// A stroked 2x2 grid: the `re` contributes its four border edges in
    /// construction order (bottom, right, top, left — the order rendering's
    /// path builder decomposes `re` into), then the two inner dividers in
    /// stream order, all at the default 1.0 line width.
    #[test]
    fn stroked_grid_yields_rulings_with_correct_endpoints() {
        let rulings = rulings_of("72 600 200 100 re S 172 600 m 172 700 l S 72 650 m 272 650 l S");
        assert_eq!(rulings.len(), 6, "{rulings:?}");
        assert_ruling(&rulings[0], 72.0, 600.0, 272.0, 600.0);
        assert_ruling(&rulings[1], 272.0, 600.0, 272.0, 700.0);
        assert_ruling(&rulings[2], 72.0, 700.0, 272.0, 700.0);
        assert_ruling(&rulings[3], 72.0, 600.0, 72.0, 700.0);
        assert_ruling(&rulings[4], 172.0, 600.0, 172.0, 700.0);
        assert_ruling(&rulings[5], 72.0, 650.0, 272.0, 650.0);
        assert!(rulings.iter().all(|r| (r.width - 1.0).abs() < 1e-3));
    }

    #[test]
    fn w_sets_the_stroke_width() {
        let rulings = rulings_of("0.5 w 72 700 m 272 700 l S");
        assert_eq!(rulings.len(), 1);
        assert!((rulings[0].width - 0.5).abs() < 1e-3);
    }

    /// A negative or non-finite `w` operand leaves the line width alone, the
    /// way the renderer treats it. The 39-digit literal lexes as a real and
    /// overflows `f32` to infinity; unguarded, that width would fail the
    /// segment gate and silently drop the stroke.
    #[test]
    fn negative_or_nonfinite_w_is_ignored() {
        let rulings = rulings_of("-5 w 72 700 m 272 700 l S");
        assert_eq!(rulings.len(), 1, "{rulings:?}");
        assert!((rulings[0].width - 1.0).abs() < 1e-3);
        let rulings = rulings_of("400000000000000000000000000000000000000 w 72 700 m 272 700 l S");
        assert_eq!(rulings.len(), 1, "{rulings:?}");
        assert!((rulings[0].width - 1.0).abs() < 1e-3);
    }

    #[test]
    fn ext_gstate_lw_sets_the_stroke_width() {
        use pdfboss_testkit::PdfBuilder;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /ExtGState << /G1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/G1 gs 72 700 m 272 700 l S");
        b.object(5, "<< /Type /ExtGState /LW 2.5 >>");
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let rulings = page_rulings(&doc, &page);
        assert_eq!(rulings.len(), 1);
        assert!((rulings[0].width - 2.5).abs() < 1e-3);
    }

    #[test]
    fn thin_filled_rect_yields_its_centerline() {
        let rulings = rulings_of("72 700 200 0.8 re f");
        assert_eq!(rulings.len(), 1, "{rulings:?}");
        assert_ruling(&rulings[0], 72.0, 700.4, 272.0, 700.4);
        assert_eq!(rulings[0].width, 0.0, "a fill has no stroke width");
    }

    #[test]
    fn fat_filled_rect_yields_no_rulings() {
        assert!(rulings_of("72 600 200 40 re f").is_empty());
    }

    /// Axis alignment is judged after the CTM: a 90° rotation turns a
    /// horizontal segment into a vertical ruling, while a 30° rotation
    /// leaves it diagonal and drops it.
    #[test]
    fn cm_rotation_keeps_axis_aligned_segments_only() {
        let rotated90 = rulings_of("q 0 1 -1 0 300 100 cm 0 0 m 100 0 l S Q");
        assert_eq!(rotated90.len(), 1, "{rotated90:?}");
        assert_ruling(&rotated90[0], 300.0, 100.0, 300.0, 200.0);
        let rotated30 = rulings_of("q 0.866 0.5 -0.5 0.866 0 0 cm 72 700 m 172 700 l S Q");
        assert!(rotated30.is_empty(), "{rotated30:?}");
    }

    /// The curve poisons its own subpath — including the straight `l` that
    /// continues it — but not the sibling subpath committed by the same `S`.
    #[test]
    fn curves_poison_only_their_own_subpath() {
        let rulings =
            rulings_of("72 500 m 100 550 150 550 172 500 c 200 500 l 72 700 m 272 700 l S");
        assert_eq!(rulings.len(), 1, "{rulings:?}");
        assert_ruling(&rulings[0], 72.0, 700.0, 272.0, 700.0);
    }

    /// `n` discards the path whether it stands alone or finishes a `W`
    /// clip: clipping never commits rulings.
    #[test]
    fn end_path_discards_the_accumulated_path() {
        assert!(rulings_of("72 700 m 272 700 l n").is_empty());
        assert!(rulings_of("72 600 200 100 re W n").is_empty());
    }

    /// A form's `/Matrix` concatenates into the CTM its content runs under,
    /// so its rulings land in page space like its spans do.
    #[test]
    fn form_matrix_lands_rulings_in_page_space() {
        use pdfboss_testkit::PdfBuilder;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /XObject << /Fx 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.stream(
            5,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Matrix [1 0 0 1 0 -20]",
            b"72 720 m 272 720 l S",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let rulings = page_rulings(&doc, &page);
        assert_eq!(rulings.len(), 1, "{rulings:?}");
        assert_ruling(&rulings[0], 72.0, 700.0, 272.0, 700.0);
    }
}
