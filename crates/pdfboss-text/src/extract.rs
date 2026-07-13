//! Content-op execution with full text state (Tm/Tlm, Tf, Tc, Tw, Tz, TL,
//! Ts), glyph advances, form XObject recursion, and the line/word layout
//! pass.

use crate::font::Font;
use pdfboss_core::content::{parse_content, Op, TextItem};
use pdfboss_core::{Dict, Document, Matrix, Page, Point, Result};
use std::collections::HashMap;
use std::rc::Rc;

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
pub fn page_spans(doc: &Document, page: &Page) -> Result<Vec<RawSpan>> {
    let content = page.content(doc)?;
    let ops = parse_content(&content)?;
    let mut exec = Executor {
        doc,
        spans: Vec::new(),
        fallback: Rc::new(Font::fallback()),
        forms: 0,
    };
    exec.run(&ops, &page.resources, GState::new(), 0);
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
    font: Option<Rc<Font>>,
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

struct Executor<'a> {
    doc: &'a Document,
    spans: Vec<RawSpan>,
    fallback: Rc<Font>,
    /// Form-XObject invocations so far, checked against
    /// `MAX_FORM_INVOCATIONS`.
    forms: usize,
}

impl Executor<'_> {
    /// Loads (with per-stream caching) the font resource `name` from the
    /// active resource dictionary, falling back to a default font.
    fn font(
        &self,
        resources: &Dict,
        name: &str,
        cache: &mut HashMap<String, Rc<Font>>,
    ) -> Rc<Font> {
        if let Some(f) = cache.get(name) {
            return f.clone();
        }
        let loaded = resources
            .get("Font")
            .and_then(|o| self.doc.resolve(o).ok())
            .and_then(|o| o.as_dict().cloned())
            .and_then(|fonts| {
                let entry = fonts.get(name)?;
                let resolved = self.doc.resolve(entry).ok()?;
                let dict = resolved.as_dict()?;
                Some(Rc::new(Font::load(self.doc, dict)))
            })
            .unwrap_or_else(|| self.fallback.clone());
        cache.insert(name.to_string(), loaded.clone());
        loaded
    }

    /// Executes one operator stream. `q`/`Q` and `cm` maintain the CTM;
    /// text operators maintain Tm/Tlm; shown strings become spans.
    fn run(&mut self, ops: &[Op], resources: &Dict, initial: GState, depth: usize) {
        let mut gs = initial;
        let mut stack: Vec<GState> = Vec::new();
        let mut tm = Matrix::identity();
        let mut tlm = Matrix::identity();
        let mut fonts: HashMap<String, Rc<Font>> = HashMap::new();
        for op in ops {
            match op {
                Op::Save => stack.push(gs.clone()),
                Op::Restore => {
                    if let Some(saved) = stack.pop() {
                        gs = saved;
                    }
                }
                Op::Concat(m) if finite(m) => gs.ctm = m.concat(gs.ctm),
                Op::BeginText => {
                    tm = Matrix::identity();
                    tlm = Matrix::identity();
                }
                Op::SetCharSpacing(v) => gs.char_spacing = *v,
                Op::SetWordSpacing(v) => gs.word_spacing = *v,
                Op::SetHorizScaling(v) => gs.horiz_scale = v / 100.0,
                Op::SetLeading(v) => gs.leading = *v,
                Op::SetTextRise(v) => gs.rise = *v,
                Op::SetFont(name, size) => {
                    gs.font = Some(self.font(resources, &name.0, &mut fonts));
                    gs.font_name = name.0.clone();
                    gs.size = *size;
                }
                Op::TextMove(tx, ty) => {
                    tlm = Matrix::translate(*tx, *ty).concat(tlm);
                    tm = tlm;
                }
                Op::TextMoveSetLeading(tx, ty) => {
                    gs.leading = -ty;
                    tlm = Matrix::translate(*tx, *ty).concat(tlm);
                    tm = tlm;
                }
                Op::SetTextMatrix(m) if finite(m) => {
                    tm = *m;
                    tlm = *m;
                }
                Op::TextNextLine => {
                    tlm = Matrix::translate(0.0, -gs.leading).concat(tlm);
                    tm = tlm;
                }
                Op::ShowText(s) => self.show(&gs, &mut tm, s),
                Op::ShowTextAdjusted(items) => {
                    for item in items {
                        match item {
                            TextItem::Str(s) => self.show(&gs, &mut tm, s),
                            TextItem::Offset(n) => {
                                let tx = -n / 1000.0 * gs.size * gs.horiz_scale;
                                if tx.is_finite() {
                                    tm = Matrix::translate(tx, 0.0).concat(tm);
                                }
                            }
                        }
                    }
                }
                Op::NextLineShowText(s) => {
                    tlm = Matrix::translate(0.0, -gs.leading).concat(tlm);
                    tm = tlm;
                    self.show(&gs, &mut tm, s);
                }
                Op::NextLineShowTextSpaced(aw, ac, s) => {
                    gs.word_spacing = *aw;
                    gs.char_spacing = *ac;
                    tlm = Matrix::translate(0.0, -gs.leading).concat(tlm);
                    tm = tlm;
                    self.show(&gs, &mut tm, s);
                }
                Op::XObject(name) => self.form_xobject(&name.0, resources, &gs, depth),
                // Text render mode 3 (invisible) is still extracted, so
                // `Tr` and everything else is a no-op here.
                _ => {}
            }
        }
    }

    /// Shows one string: decodes each code, advances the text matrix by
    /// `(w/1000·Tfs + Tc + Tw[code 32]) · Tz/100`, and emits a span whose
    /// origin is `(0, Ts)` under `Tm · CTM`.
    fn show(&mut self, gs: &GState, tm: &mut Matrix, bytes: &[u8]) {
        let font = gs.font.clone().unwrap_or_else(|| self.fallback.clone());
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
        if !text.is_empty() && origin.x.is_finite() && origin.y.is_finite() {
            self.spans.push(RawSpan {
                text,
                x: origin.x,
                y: origin.y,
                end_x: end.x,
                size: if size.is_finite() { size } else { 0.0 },
                font: gs.font_name.clone(),
            });
        }
    }

    /// Executes a form XObject: recurses into its content with its own
    /// `/Resources` (falling back to the caller's), `/Matrix` prepended
    /// to the CTM, a depth cap, and a total-invocation budget.
    fn form_xobject(&mut self, name: &str, resources: &Dict, gs: &GState, depth: usize) {
        if depth >= MAX_FORM_DEPTH || self.forms >= MAX_FORM_INVOCATIONS {
            return;
        }
        self.forms += 1;
        let Some(stream) = resources
            .get("XObject")
            .and_then(|o| self.doc.resolve(o).ok())
            .and_then(|o| o.as_dict().cloned())
            .and_then(|xd| {
                let entry = xd.get(name)?;
                self.doc.resolve(entry).ok()
            })
            .and_then(|o| o.as_stream().cloned())
        else {
            return;
        };
        let is_form = stream
            .dict
            .get_name("Subtype")
            .is_some_and(|n| n.0 == "Form");
        if !is_form {
            return; // images and other XObjects carry no text
        }
        let Ok(data) = self.doc.stream_data(&stream) else {
            return;
        };
        let Ok(ops) = parse_content(&data) else {
            return;
        };
        let form_resources = stream
            .dict
            .get("Resources")
            .and_then(|o| self.doc.resolve(o).ok())
            .and_then(|o| o.as_dict().cloned())
            .unwrap_or_else(|| resources.clone());
        let mut inner = gs.clone();
        if let Some(m) = form_matrix(self.doc, &stream.dict) {
            inner.ctm = m.concat(inner.ctm);
        }
        self.run(&ops, &form_resources, inner, depth + 1);
    }
}

/// Reads a `/Matrix` entry (six numbers) from a form XObject dictionary.
fn form_matrix(doc: &Document, dict: &Dict) -> Option<Matrix> {
    let obj = doc.resolve(dict.get("Matrix")?).ok()?;
    let arr = obj.as_array()?;
    let mut v = [0.0f32; 6];
    for (slot, item) in v.iter_mut().zip(arr.iter()) {
        *slot = doc.resolve(item).ok()?.as_f64()? as f32;
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
    use pdfboss_testkit::doc_with_graphics;

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
}
