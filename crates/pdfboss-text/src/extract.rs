//! Content-op execution with full text state (Tm/Tlm, Tf, Tc, Tw, Tz, TL,
//! Ts), glyph advances, form XObject recursion, and the line/word layout
//! pass.

use crate::font::Font;
use pdfboss_core::content::{parse_content, Op, TextItem};
use pdfboss_core::{
    page_content_with, AsyncObjectSource, Dict, Matrix, Object, Page, Point, Result,
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

/// Runs the page's content stream (and any form XObjects) and collects
/// every shown string as a [`RawSpan`], in emission order.
///
/// The source is taken by value so that the returned future can be `'static`;
/// `page` is borrowed, which does not stand in the way, because a caller that
/// owns its page creates the borrow inside its own `async move` block. See
/// `pdfboss_core::source`'s "Signing a shared algorithm".
pub async fn page_spans_with<S: AsyncObjectSource>(src: S, page: &Page) -> Result<Vec<RawSpan>> {
    let content = page_content_with(&src, page).await?;
    let ops = parse_content(&content)?;
    let mut exec = Executor {
        src: &src,
        spans: Vec::new(),
        fallback: Arc::new(Font::fallback()),
        forms: 0,
    };
    let root = Frame::new(
        ops.into(),
        vec![Arc::new(page.resources.clone())],
        GState::new(),
        0,
    );
    exec.run(root).await;
    Ok(exec.spans)
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
    /// `None` wherever the recursive version simply returned: over budget, no
    /// such resource, not a form, or content that will not parse. The
    /// invocation is counted before any of those checks, exactly as before, so
    /// a page of unreadable forms still exhausts its budget.
    async fn form_frame(
        &mut self,
        name: &str,
        chain: &[Arc<Dict>],
        gs: &GState,
        depth: usize,
    ) -> Option<Frame> {
        if depth >= MAX_FORM_DEPTH || self.forms >= MAX_FORM_INVOCATIONS {
            return None;
        }
        self.forms += 1;
        let stream = self
            .find_res(chain, "XObject", name)
            .await
            .and_then(|o| o.as_stream().cloned())?;
        let is_form = stream
            .dict
            .get_name("Subtype")
            .is_some_and(|n| n.0 == "Form");
        if !is_form {
            return None; // images and other XObjects carry no text
        }
        let data = self.src.stream_data(&stream).await.ok()?;
        let ops = parse_content(&data).ok()?;
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

/// Groups spans into lines (baselines within `0.5 · size`), orders lines
/// top to bottom and spans left to right, inserts a space at horizontal
/// gaps wider than `0.25 · size`, and joins lines with `\n`.
pub fn layout(spans: &[RawSpan]) -> String {
    struct Line<'s> {
        y: f32,
        size: f32,
        spans: Vec<&'s RawSpan>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for span in spans {
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
    let mut out = String::new();
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
                if gap > 0.25 * prev_size.max(span.size) {
                    out.push(' ');
                }
            }
            out.push_str(&span.text);
            prev_end = Some(span.end_x);
            prev_size = span.size;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{block_on, Document, Immediate};
    use pdfboss_testkit::doc_with_graphics;

    /// The synchronous spans accessor. Production has no use for one — the public
    /// entry points in `lib.rs` wrap [`page_spans_with`] themselves — but it is
    /// the same `block_on` over `Immediate`, so every test below still asserts on
    /// exactly what a synchronous caller receives.
    fn page_spans(doc: &Document, page: &Page) -> Result<Vec<RawSpan>> {
        block_on(page_spans_with(Immediate(doc), page))
    }

    /// Extracted, laid-out text of a one-page document with `content` as
    /// its raw content stream (12pt /F1 with default widths of 500).
    fn text_of(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        layout(&page_spans(&doc, &page).unwrap())
    }

    /// Raw spans of the same setup.
    fn spans_of(content: &str) -> Vec<RawSpan> {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        page_spans(&doc, &page).unwrap()
    }

    #[test]
    fn two_td_lines_become_newline() {
        let text = text_of("BT /F1 12 Tf 72 720 Td (Line one) Tj 0 -20 Td (Line two) Tj ET");
        assert_eq!(text, "Line one\nLine two");
    }

    #[test]
    fn tj_offset_space_thresholds() {
        // -300/1000 * 12 = 3.6 > 0.25 * 12 -> space.
        assert_eq!(
            text_of("BT /F1 12 Tf 72 720 Td [(A) -300 (B)] TJ ET"),
            "A B"
        );
        // -50/1000 * 12 = 0.6 -> no space.
        assert_eq!(text_of("BT /F1 12 Tf 72 720 Td [(A) -50 (B)] TJ ET"), "AB");
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
        let spans = page_spans(&doc, &page).unwrap();
        assert!(!spans.is_empty()); // nested forms still extract text
        assert!(
            spans.len() <= MAX_FORM_INVOCATIONS,
            "fan-out not bounded: {} spans",
            spans.len()
        );
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
        let spans = page_spans(&doc, &page).unwrap();
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
