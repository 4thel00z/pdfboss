//! Text extraction for pdfboss: font loading, encodings, ToUnicode CMaps,
//! and positional text spans.

mod cmap;
mod extract;
mod font;
mod sfnt;

use pdfboss_core::{block_on, AsyncObjectSource, Document, Immediate, OcState, Page, Result};

pub use extract::{ExtractReport, FontCache, SkipCause, SkippedText, SkippedTextKind};
pub use pdfboss_core::{Point, Rect};

/// A positioned run of extracted text.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpan {
    /// The decoded text.
    pub text: String,
    /// Device-space x coordinate of the span origin.
    pub x: f32,
    /// Device-space y coordinate of the span baseline.
    pub y: f32,
    /// Device-space x after the last glyph's advance.
    pub end_x: f32,
    /// Effective font size.
    pub size: f32,
    /// Font resource name.
    pub font: String,
    /// The font's `/BaseFont` name verbatim — subset prefix included —
    /// falling back to the FontDescriptor's `/FontName`; empty when the
    /// file names the font nowhere (a missing font resource included).
    pub font_name: String,
    /// 0-based index of the page the span came from.
    pub page: usize,
    /// Device-space box: origin to advance horizontally, the font's
    /// `/Descent`..`/Ascent` (per-mille of the effective size) vertically.
    /// Exact for unrotated horizontal text, an approximation under rotated
    /// matrices; vertical writing takes the advance as its vertical extent
    /// and half the size to each side of the baseline.
    pub bbox: Rect,
    /// Whether the font that produced this span is bold: FontDescriptor
    /// `/FontWeight` >= 600, `/Flags` ForceBold, or a `/StemV` in bold
    /// stem-width territory, else a `Bold` substring
    /// in `/BaseFont` (ISO 32000-1 Table 123).
    pub bold: bool,
    /// Whether the font that produced this span is italic: FontDescriptor
    /// `/Flags` Italic or a nonzero `/ItalicAngle`, else an `Italic` or
    /// `Oblique` substring in `/BaseFont` (ISO 32000-1 Table 123).
    pub italic: bool,
    /// FontDescriptor `/Flags` FixedPitch (ISO 32000-1 Table 123 bit 1).
    pub monospace: bool,
    /// FontDescriptor `/Flags` Serif (ISO 32000-1 Table 123 bit 2).
    pub serif: bool,
    /// The text rise (`Ts`) the span was shown under, in unscaled text
    /// space: positive above the baseline — a superscript/subscript
    /// signal. The origin already includes the shift.
    pub rise: f32,
    /// Writing mode 1: the text advances downward and `bbox` takes the
    /// advance as its vertical extent.
    pub vertical: bool,
    /// Shown under render mode 3 or 7 (ISO 32000-1 Table 106), which paint
    /// nothing — the shape of an OCR text layer under a scanned image.
    pub invisible: bool,
    /// The fill color the span was shown with, as RGB in `[0, 1]`. Device
    /// gray/RGB/CMYK convert exactly; other spaces' components are read by
    /// count (1 gray, 3 RGB, 4 CMYK) without running the space's
    /// transform. `None` for pattern fills, which have no single color.
    pub color: Option<(f32, f32, f32)>,
    /// A drawn ruling sits just below the baseline and covers most of the
    /// span. PDF has no underline attribute — this is read from the page's
    /// geometry, so a table border hugging a cell's text can read as one.
    pub underline: bool,
    /// A drawn ruling crosses the span's x-height band — geometry-read,
    /// like `underline`.
    pub strikethrough: bool,
}

/// An axis-aligned line segment a page draws, in the same y-up user space as
/// `TextSpan`: a table border, a separator, an underline.
///
/// Endpoints are normalized (`start.x <= end.x`, `start.y <= end.y`) and
/// exactly axis-aligned: the near-constant coordinate is snapped to its
/// midpoint over the segment.
#[derive(Debug, Clone, PartialEq)]
pub struct Ruling {
    pub start: Point,
    pub end: Point,
    /// Stroke width in device space. Zero does not say how the ruling was
    /// drawn: a hairline stroke (`0 w`) and a thin filled rectangle's
    /// centerline both carry 0.0.
    pub width: f32,
}

/// Extracts the page's raw text spans (position, size and font per span).
///
/// Lenient the way rendering is: content that will not fetch, decode, or
/// parse yields no spans rather than an error, so one unreadable stream
/// never costs a caller the rest of the document. Use
/// [`extract_spans_reporting`] to see what (if anything) was left out.
///
/// Content in optional-content layers the document's default configuration
/// turns off (ISO 32000-1 §8.11) is excluded, counted in
/// [`ExtractReport::hidden`]. The document-level entry points here read
/// that configuration themselves; the source-generic `_with` twins take it
/// as their `oc` parameter (`None` extracts every layer).
pub fn extract_spans(doc: &Document, page: &Page) -> Result<Vec<TextSpan>> {
    let oc = doc.oc_state();
    let (spans, _, _) = block_on(extract::page_spans_and_rulings_with(
        Immediate(doc),
        page,
        None,
        oc.as_ref(),
    ));
    Ok(spans)
}

/// [`extract_spans`] against any object source, awaiting whatever I/O the
/// source needs to read the page.
///
/// This is the shared implementation [`extract_spans`] drives over
/// [`Immediate`] on the calling thread. `oc` is the document's
/// optional-content visibility — `Document::oc_state` sync, the async
/// document's `oc_state()` over a range-fetching source — and gates hidden
/// layers exactly as the document-level entry does; `None` extracts every
/// layer.
///
/// The source is taken by value and the page by reference. That combination is
/// what a consumer needs to spawn the result: the future is `Send` over a source
/// that is `Send + Sync`, and `'static` as long as the borrow of `page` is
/// created inside the consumer's own `async move` block, which owns the page.
/// See `pdfboss_core::source`'s "Signing a shared algorithm".
pub async fn extract_spans_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    oc: Option<&OcState>,
) -> Result<Vec<TextSpan>> {
    let (spans, _, _) = extract::page_spans_and_rulings_with(src, page, None, oc).await;
    Ok(spans)
}

/// [`extract_spans`] with the report of what could not be read: an
/// [`ExtractReport`] whose entries name each skipped stream and why —
/// unsupported filters (the passthrough image codecs included), undecodable
/// bytes, unparseable content, missing resources, exhausted form limits.
/// An empty span list with an empty report really is an empty page.
pub fn extract_spans_reporting(
    doc: &Document,
    page: &Page,
) -> Result<(Vec<TextSpan>, ExtractReport)> {
    let oc = doc.oc_state();
    let (spans, _, report) = block_on(extract::page_spans_and_rulings_with(
        Immediate(doc),
        page,
        None,
        oc.as_ref(),
    ));
    Ok((spans, report))
}

/// [`extract_spans_reporting`] against any object source. Signed like
/// [`extract_spans_with`], for the same reasons — `oc` gating included.
pub async fn extract_spans_reporting_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    oc: Option<&OcState>,
) -> Result<(Vec<TextSpan>, ExtractReport)> {
    let (spans, _, report) = extract::page_spans_and_rulings_with(src, page, None, oc).await;
    Ok((spans, report))
}

/// [`extract_spans_reporting`] with fonts cached across calls: a caller
/// walking a whole document passes one [`FontCache`] to every page, and each
/// font dictionary — descriptor, widths, encoding, ToUnicode and font-program
/// parsing included — loads once for the document instead of once per page.
/// The cache is `Send + Sync`, so a parallel page walk may share it.
///
/// The result is identical to calling [`extract_spans_reporting`] per page:
/// the cache is keyed by each font dictionary's object reference, never by
/// its resource name, and a reference resolves to the same dictionary on
/// every page of a document.
pub fn extract_spans_reporting_cached(
    doc: &Document,
    page: &Page,
    fonts: &FontCache,
) -> Result<(Vec<TextSpan>, ExtractReport)> {
    let oc = doc.oc_state();
    let (spans, _, report) = block_on(extract::page_spans_and_rulings_with(
        Immediate(doc),
        page,
        Some(fonts),
        oc.as_ref(),
    ));
    Ok((spans, report))
}

/// [`extract_spans_reporting_cached`] against any object source. Signed like
/// [`extract_spans_with`], for the same reasons — `oc` gating included.
pub async fn extract_spans_reporting_cached_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    fonts: &FontCache,
    oc: Option<&OcState>,
) -> Result<(Vec<TextSpan>, ExtractReport)> {
    let (spans, _, report) = extract::page_spans_and_rulings_with(src, page, Some(fonts), oc).await;
    Ok((spans, report))
}

/// [`extract_spans_reporting`] plus the page's rulings: every axis-aligned
/// segment the content strokes, and the centerline of every thin filled
/// rectangle, in the same y-up user space as the spans. See [`Ruling`] for
/// the normalization the returned segments carry.
pub fn extract_spans_and_rulings_reporting(
    doc: &Document,
    page: &Page,
) -> Result<(Vec<TextSpan>, Vec<Ruling>, ExtractReport)> {
    let oc = doc.oc_state();
    let (spans, rulings, report) = block_on(extract::page_spans_and_rulings_with(
        Immediate(doc),
        page,
        None,
        oc.as_ref(),
    ));
    Ok((spans, rulings, report))
}

/// [`extract_spans_and_rulings_reporting`] against any object source. Signed
/// like [`extract_spans_with`], for the same reasons — `oc` gating included.
pub async fn extract_spans_and_rulings_reporting_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    oc: Option<&OcState>,
) -> Result<(Vec<TextSpan>, Vec<Ruling>, ExtractReport)> {
    let (spans, rulings, report) = extract::page_spans_and_rulings_with(src, page, None, oc).await;
    Ok((spans, rulings, report))
}

/// [`extract_spans_and_rulings_reporting`] with fonts cached across calls —
/// the rulings twin of [`extract_spans_reporting_cached`], for a caller
/// walking a whole document page by page. Spans, rulings, and report are
/// identical to the uncached call's, for the same reason: the cache is keyed
/// by each font dictionary's object reference, never by its resource name,
/// and rulings never touch fonts at all.
pub fn extract_spans_and_rulings_reporting_cached(
    doc: &Document,
    page: &Page,
    fonts: &FontCache,
) -> Result<(Vec<TextSpan>, Vec<Ruling>, ExtractReport)> {
    let oc = doc.oc_state();
    let (spans, rulings, report) = block_on(extract::page_spans_and_rulings_with(
        Immediate(doc),
        page,
        Some(fonts),
        oc.as_ref(),
    ));
    Ok((spans, rulings, report))
}

/// [`extract_spans_and_rulings_reporting_cached`] against any object source.
/// Signed like [`extract_spans_with`], for the same reasons — `oc` gating
/// included.
pub async fn extract_spans_and_rulings_reporting_cached_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    fonts: &FontCache,
    oc: Option<&OcState>,
) -> Result<(Vec<TextSpan>, Vec<Ruling>, ExtractReport)> {
    let (spans, rulings, report) =
        extract::page_spans_and_rulings_with(src, page, Some(fonts), oc).await;
    Ok((spans, rulings, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{resolve_with, BoxFuture, ObjRef, Object, Stream};
    use pdfboss_testkit::{simple_doc, PdfBuilder};
    use std::future::Future;

    /// A form's `/Matrix` translates the CTM under which its content runs
    /// (ISO 32000-1 §8.10.2): the nested span's baseline lands at the
    /// page-space position the outer text's `Td` moved to, offset by the
    /// form's own translation, not at the form's local coordinates.
    #[test]
    fn form_matrix_translates_the_nested_span() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (out) Tj ET /Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        // No own /Resources: falls back to the page's, so /F1 resolves.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Matrix [1 0 0 1 0 -20]",
            b"BT /F1 12 Tf 72 720 Td (in) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans.len(), 2);
        assert!((spans[1].y - 700.0).abs() < 1e-3); // form matrix applied
    }

    #[test]
    fn extract_spans_sane_positions() {
        let doc = Document::load(simple_doc("Hi")).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.text, "Hi");
        assert!((s.x - 72.0).abs() < 1e-3);
        assert!((s.y - 720.0).abs() < 1e-3);
        assert!((s.size - 12.0).abs() < 1e-3);
        assert_eq!(s.font, "F1");
    }

    /// The combined entry point carries the spans, the drawn rulings, and
    /// the completeness report through in one call.
    #[test]
    fn extract_spans_and_rulings_reports_both() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td (Hi) Tj ET 72 700 m 272 700 l S",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let (spans, rulings, report) = extract_spans_and_rulings_reporting(&doc, &page).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "Hi");
        assert_eq!(rulings.len(), 1);
        assert!((rulings[0].start.y - 700.0).abs() < 1e-3);
        assert!(report.is_complete());
    }

    #[test]
    fn extract_spans_ordering_multi_line() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td (top) Tj 0 -40 Td (bottom) Tj ET",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans.len(), 2);
        assert!(spans[0].y > spans[1].y);
        assert_eq!(spans[0].text, "top");
        assert_eq!(spans[1].text, "bottom");
        assert!(spans.iter().all(|s| s.size > 0.0 && s.x >= 0.0));
    }

    /// `font_name` carries the file's `/BaseFont` verbatim — subset prefix
    /// included — while `font` stays the resource name.
    #[test]
    fn span_font_name_is_the_base_font_verbatim() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (x) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /ABCDEF+Times-Roman \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans[0].font_name, "ABCDEF+Times-Roman");
        assert_eq!(spans[0].font, "F1");
    }

    /// A font dictionary with no `/BaseFont` falls back to the descriptor's
    /// `/FontName`; a missing or unloadable font resource yields an empty
    /// name.
    #[test]
    fn span_font_name_falls_back_to_descriptor_font_name() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R /F2 7 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (a) Tj /F2 12 Tf (b) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /Encoding /WinAnsiEncoding \
             /FontDescriptor 6 0 R >>",
        );
        b.object(6, "<< /Type /FontDescriptor /FontName /Nameless-Face >>");
        b.object(
            7,
            "<< /Type /Font /Subtype /Type1 /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans[0].font_name, "Nameless-Face");
        assert_eq!(spans[1].font_name, "");
    }

    /// Every span names the 0-based page it came from.
    #[test]
    fn span_carries_its_page_index() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (one) Tj ET");
        b.object(
            5,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>",
        );
        b.stream(6, "", b"BT /F1 12 Tf 72 720 Td (two) Tj ET");
        b.object(
            7,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        for index in 0..2 {
            let page = doc.page(index).unwrap();
            let spans = extract_spans(&doc, &page).unwrap();
            assert_eq!(spans[0].page, index, "page {index}");
        }
    }

    /// The bbox spans origin to advance horizontally and the descriptor's
    /// `/Descent`..`/Ascent` vertically, both scaled by the effective size.
    #[test]
    fn span_bbox_uses_descriptor_metrics() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (Hi) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /Helvetica \
             /Ascent 718 /Descent -207 >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        let s = &spans[0];
        assert!((s.bbox.x0 - s.x).abs() < 1e-3);
        assert!((s.bbox.x1 - s.end_x).abs() < 1e-3);
        assert!((s.bbox.y0 - (720.0 - 0.207 * 12.0)).abs() < 1e-3);
        assert!((s.bbox.y1 - (720.0 + 0.718 * 12.0)).abs() < 1e-3);
    }

    /// Without a descriptor the vertical extent falls back to 0.8 em above
    /// and 0.2 em below the baseline.
    #[test]
    fn span_bbox_defaults_to_em_fractions() {
        let doc = Document::load(simple_doc("Hi")).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        let s = &spans[0];
        assert!((s.bbox.y0 - (720.0 - 0.2 * 12.0)).abs() < 1e-3);
        assert!((s.bbox.y1 - (720.0 + 0.8 * 12.0)).abs() < 1e-3);
    }

    /// A descriptor stating `/CapHeight` but no `/Ascent` uses it for the
    /// upper edge.
    #[test]
    fn span_bbox_falls_back_to_cap_height() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (Hi) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /Helvetica /CapHeight 700 >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!((spans[0].bbox.y1 - (720.0 + 0.7 * 12.0)).abs() < 1e-3);
        assert!((spans[0].bbox.y0 - (720.0 - 0.2 * 12.0)).abs() < 1e-3);
    }

    /// Table 123 bit 1 (FixedPitch) surfaces as `monospace`.
    #[test]
    fn fixed_pitch_flag_marks_monospace() {
        let spans = flag_spans(1);
        assert!(spans[0].monospace);
        assert!(!spans[0].serif);
    }

    /// Table 123 bit 2 (Serif) surfaces as `serif`.
    #[test]
    fn serif_flag_marks_serif() {
        let spans = flag_spans(2);
        assert!(spans[0].serif);
        assert!(!spans[0].monospace);
    }

    /// One page shown with a font whose descriptor states `/Flags flags`.
    fn flag_spans(flags: u32) -> Vec<TextSpan> {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (x) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Custom \
             /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
        );
        b.object(
            6,
            &format!("<< /Type /FontDescriptor /FontName /Custom /Flags {flags} >>"),
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        extract_spans(&doc, &page).unwrap()
    }

    /// The span records the text rise (`Ts`) it was shown under.
    #[test]
    fn span_carries_text_rise() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td (flat) Tj 5 Ts (up) Tj ET",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans[0].rise, 0.0);
        assert_eq!(spans[1].rise, 5.0);
    }

    /// A writing-mode-1 (`Identity-V`) font marks its spans vertical.
    #[test]
    fn span_marks_vertical_writing() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td <0001> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-V \
             /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /DW 600 >>",
        );
        b.stream(
            7,
            "",
            b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
              1 beginbfchar <0001> <0041> endbfchar",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(spans[0].vertical);
    }

    /// Render modes 3 and 7 paint nothing (ISO 32000-1 Table 106) — the
    /// shape of an OCR text layer — and mark the span invisible; a later
    /// `Tr` back to a painting mode clears the mark.
    #[test]
    fn invisible_render_modes_mark_the_span() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td (seen) Tj 3 Tr (ocr) Tj 7 Tr (clip) Tj 0 Tr (back) Tj ET",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        let invisible: Vec<bool> = spans.iter().map(|s| s.invisible).collect();
        assert_eq!(invisible, [false, true, true, false]);
    }

    /// The fill color defaults to black (ISO 32000-1 §8.6.8).
    #[test]
    fn span_color_defaults_to_black() {
        let doc = Document::load(simple_doc("Hi")).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans[0].color, Some((0.0, 0.0, 0.0)));
    }

    /// `rg`, `g` and `k` set the span color, CMYK and gray converted to RGB.
    #[test]
    fn device_fill_colors_set_the_span_color() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td 1 0 0 rg (red) Tj 0.5 g (gray) Tj \
             1 0 0 0 k (cyan) Tj ET",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans[0].color, Some((1.0, 0.0, 0.0)));
        assert_eq!(spans[1].color, Some((0.5, 0.5, 0.5)));
        assert_eq!(spans[2].color, Some((0.0, 1.0, 1.0)));
    }

    /// `sc`/`scn` components are read by count — 1 gray, 3 RGB, 4 CMYK —
    /// whatever the named space, the same approximation every extractor
    /// makes without running the space's transform.
    #[test]
    fn sc_components_set_the_span_color_by_count() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td /DeviceRGB cs 0 1 0 sc (green) Tj \
             0.25 sc (dark) Tj ET",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans[0].color, Some((0.0, 1.0, 0.0)));
        assert_eq!(spans[1].color, Some((0.25, 0.25, 0.25)));
    }

    /// A pattern fill has no single color: the span says so with `None`
    /// rather than guessing.
    #[test]
    fn pattern_fill_leaves_color_unknown() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td /Pattern cs /P1 scn (patterned) Tj ET",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert_eq!(spans[0].color, None);
    }

    /// A ruling drawn just below the baseline, covering the span, reads as
    /// an underline.
    #[test]
    fn an_underline_ruling_marks_the_span() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td (Hello) Tj ET 72 718.5 m 105 718.5 l S",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(spans[0].underline);
        assert!(!spans[0].strikethrough);
    }

    /// A ruling crossing the x-height band reads as a strikethrough.
    #[test]
    fn a_strikethrough_ruling_marks_the_span() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td (Hello) Tj ET 72 723.6 m 105 723.6 l S",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(spans[0].strikethrough);
        assert!(!spans[0].underline);
    }

    /// A ruling far from the baseline — a table border, a separator —
    /// decorates nothing.
    #[test]
    fn a_distant_ruling_marks_nothing() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td (Hello) Tj ET 72 700 m 105 700 l S",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(!spans[0].underline);
        assert!(!spans[0].strikethrough);
    }

    /// A ruling at underline height that barely overlaps the span — a
    /// neighbour's underline continuing past a word boundary does not
    /// count; the mark needs most of the span covered.
    #[test]
    fn an_underline_needs_most_of_the_span_covered() {
        let doc = Document::load(pdfboss_testkit::doc_with_graphics(
            "BT /F1 12 Tf 72 720 Td (Hello) Tj ET 100 718.5 m 130 718.5 l S",
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(!spans[0].underline);
    }

    /// The source-generic entry points take the optional-content state a
    /// document-owning caller can read (`Document::oc_state`, or the async
    /// document's `oc_state()`), so a hidden layer is excluded over any
    /// source exactly as the document-level entries exclude it; `None`
    /// still extracts every layer.
    #[test]
    fn the_source_generic_entry_points_honor_optional_content() {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /OCProperties \
             << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >> >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> \
             /Properties << /H 6 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf 72 720 Td /OC /H BDC (hidden) Tj EMC (kept) Tj ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.object(6, "<< /Type /OCG /Name (hidden) >>");
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let oc = doc.oc_state();
        let gated = block_on(extract_spans_with(Immediate(&doc), &page, oc.as_ref())).unwrap();
        let texts: Vec<&str> = gated.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["kept"]);
        let all = block_on(extract_spans_with(Immediate(&doc), &page, None)).unwrap();
        let texts: Vec<&str> = all.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["hidden", "kept"]);
    }

    /// FontDescriptor evidence: /Flags italic bit and /FontWeight.
    /// Verify the exact bit position against ISO 32000-1 Table 123 while
    /// implementing — bit 7 (mask 64) is Italic, bit 19 (mask 0x40000) is
    /// ForceBold — and cite the table in the implementation comment.
    #[test]
    fn descriptor_flags_set_span_style() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (x) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Custom \
             /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /Custom /Flags 64 /FontWeight 700 >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(spans[0].italic, "Flags bit 7 (mask 64) is Italic");
        assert!(spans[0].bold, "FontWeight 700 >= 600 is bold");
    }

    /// Table 122 `/StemV`: a thick dominant vertical stem marks a bold face
    /// whose descriptor carries neither a weight nor a telling name — the
    /// URW `-Medi` faces LaTeX embeds. A regular-width stem stays regular.
    #[test]
    fn thick_stemv_reads_as_bold() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R /F2 7 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (a) Tj /F2 12 Tf (b) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /NimbusRomNo9L-Medi \
             /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /NimbusRomNo9L-Medi /Flags 4 /StemV 140 >>",
        );
        b.object(
            7,
            "<< /Type /Font /Subtype /Type1 /BaseFont /NimbusRomNo9L-Regu \
             /Encoding /WinAnsiEncoding /FontDescriptor 8 0 R >>",
        );
        b.object(
            8,
            "<< /Type /FontDescriptor /FontName /NimbusRomNo9L-Regu /Flags 4 /StemV 85 >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(spans[0].bold, "StemV 140 is a bold stem");
        assert!(!spans[1].bold, "StemV 85 is a regular stem");
    }

    /// BaseFont-name fallback when no descriptor exists, and ItalicAngle.
    #[test]
    fn basefont_name_and_italic_angle_fallbacks() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (a) Tj /F2 12 Tf (b) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-BoldOblique \
             /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman \
             /Encoding /WinAnsiEncoding /FontDescriptor 7 0 R >>",
        );
        b.object(
            7,
            "<< /Type /FontDescriptor /FontName /Times-Roman /ItalicAngle -12 >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(
            spans[0].bold && spans[0].italic,
            "BaseFont substrings Bold+Oblique"
        );
        assert!(!spans[1].bold && spans[1].italic, "ItalicAngle != 0 alone");
    }

    /// Type0: the descriptor hangs off the descendant font.
    #[test]
    fn type0_descendant_descriptor_sets_style() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td <0001> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
             /DescendantFonts [6 0 R] /ToUnicode 8 0 R >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /DW 600 \
             /FontDescriptor 7 0 R >>",
        );
        b.object(
            7,
            "<< /Type /FontDescriptor /FontName /X /Flags 64 /FontWeight 600 >>",
        );
        b.stream(
            8,
            "",
            b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
              1 beginbfchar <0001> <0041> endbfchar",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let spans = extract_spans(&doc, &page).unwrap();
        assert!(spans[0].bold && spans[0].italic);
    }

    /// An asynchronous source that counts each reference resolution by
    /// object number and delegates to the document. Loading a font resolves
    /// its dictionary's reference exactly once, so the count makes cache
    /// hits observable without any instrumentation in the production code.
    struct Counting<'a> {
        inner: Immediate<&'a Document>,
        resolutions: std::cell::RefCell<std::collections::HashMap<u32, usize>>,
    }

    impl<'a> Counting<'a> {
        fn new(doc: &'a Document) -> Counting<'a> {
            Counting {
                inner: Immediate(doc),
                resolutions: std::cell::RefCell::new(std::collections::HashMap::new()),
            }
        }

        fn resolutions(&self, num: u32) -> usize {
            self.resolutions.borrow().get(&num).copied().unwrap_or(0)
        }
    }

    impl AsyncObjectSource for Counting<'_> {
        fn get(&self, r: ObjRef) -> BoxFuture<'_, Result<Object>> {
            self.inner.get(r)
        }

        fn stream_data<'b>(&'b self, s: &'b Stream) -> BoxFuture<'b, Result<Vec<u8>>> {
            self.inner.stream_data(s)
        }

        fn resolve<'b>(&'b self, o: &'b Object) -> BoxFuture<'b, Result<Object>> {
            if let Object::Ref(r) = o {
                *self.resolutions.borrow_mut().entry(r.num).or_insert(0) += 1;
            }
            self.inner.resolve(o)
        }
    }

    /// Two invocations of the same form used to load the form's font twice:
    /// every invocation started with an empty font map. The walk-level cache
    /// (no [`FontCache`] involved) must fetch the font dictionary once.
    #[test]
    fn a_font_reached_from_repeated_forms_loads_once_per_page() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /XObject << /Fx 6 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do /Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >>",
            b"BT /F1 12 Tf 72 700 Td (x) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let counting = Counting::new(&doc);
        let (spans, report) =
            block_on(extract_spans_reporting_with(&counting, &page, None)).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        assert_eq!(spans.len(), 2, "both form invocations must show text");
        assert_eq!(
            counting.resolutions(5),
            1,
            "one font dictionary resolution per page walk"
        );
    }

    /// A two-page document whose pages bind the same font dictionary: with
    /// one [`FontCache`] passed to both extractions the dictionary is
    /// fetched once, and the spans are exactly the uncached call's.
    #[test]
    fn a_font_shared_across_pages_loads_once_per_document() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (one) Tj ET");
        b.object(
            5,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>",
        );
        b.stream(6, "", b"BT /F1 12 Tf 72 720 Td (two) Tj ET");
        b.object(
            7,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let fonts = FontCache::default();
        let counting = Counting::new(&doc);
        let mut cached = Vec::new();
        for index in 0..2 {
            let page = doc.page(index).unwrap();
            let (spans, report) = block_on(extract_spans_reporting_cached_with(
                &counting, &page, &fonts, None,
            ))
            .unwrap();
            assert!(report.is_complete(), "unexpected skips: {report:?}");
            cached.push(spans);
        }
        assert_eq!(
            counting.resolutions(7),
            1,
            "one font dictionary resolution per document"
        );
        for (index, spans) in cached.iter().enumerate() {
            let page = doc.page(index).unwrap();
            let plain = extract_spans_reporting(&doc, &page).unwrap().0;
            assert_eq!(spans, &plain, "page {index} must extract identically");
        }
    }

    /// `/F1` on one page and `/F1` on the next may be different fonts: the
    /// shared cache is keyed by the font dictionary's object reference, so
    /// each page keeps its own binding. A cache keyed by resource name would
    /// hand page two the font of page one and fail here.
    #[test]
    fn a_shared_cache_keeps_the_name_binding_per_page() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (aa) Tj ET");
        b.object(
            5,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 8 0 R >> >> /Contents 6 0 R >>",
        );
        b.stream(6, "", b"BT /F1 12 Tf 72 720 Td (aa) Tj ET");
        b.object(
            7,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding /FirstChar 97 /LastChar 97 /Widths [500] >>",
        );
        b.object(
            8,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding /FirstChar 97 /LastChar 97 /Widths [1000] >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let fonts = FontCache::default();
        let mut advances = Vec::new();
        for index in 0..2 {
            let page = doc.page(index).unwrap();
            let (spans, _) = extract_spans_reporting_cached(&doc, &page, &fonts).unwrap();
            assert_eq!(spans.len(), 1);
            advances.push(spans[0].end_x - spans[0].x);
        }
        assert!(
            (advances[0] - 12.0).abs() < 1e-3,
            "page one: {}",
            advances[0]
        );
        assert!(
            (advances[1] - 24.0).abs() < 1e-3,
            "page two: {}",
            advances[1]
        );
    }

    /// An asynchronous source that answers everything with `null`.
    ///
    /// The heap field is load-bearing rather than decorative. rustc const-promotes
    /// a reference to a unit struct to `&'static`, so a unit stub would satisfy
    /// the `'static` assertion below even under a signature that assertion exists
    /// to reject — a test that cannot fail. A `Vec` cannot be promoted.
    ///
    /// It is also deliberately `Send + Sync`. The helpers inside the shared
    /// implementation borrow the source across their awaits, so the owning future
    /// is `Send` only when the source is `Sync`; every genuinely asynchronous
    /// source already is, because `resolve_with` requires it.
    struct NullSource {
        payload: Vec<u8>,
    }

    impl AsyncObjectSource for NullSource {
        fn get(&self, _r: ObjRef) -> BoxFuture<'_, Result<Object>> {
            Box::pin(std::future::ready(Ok(Object::Null)))
        }

        fn stream_data<'a>(&'a self, _s: &'a Stream) -> BoxFuture<'a, Result<Vec<u8>>> {
            Box::pin(std::future::ready(Ok(self.payload.clone())))
        }

        fn resolve<'a>(&'a self, o: &'a Object) -> BoxFuture<'a, Result<Object>> {
            Box::pin(resolve_with(self, o))
        }
    }

    /// The asynchronous entry point must produce a future a runtime's `spawn`
    /// and the Python bindings will accept, which means `Send + 'static`.
    ///
    /// The `async move` block is the shape a consumer actually writes: it owns
    /// the source and the page, and the borrow of the page that
    /// `extract_spans_with` takes is created inside it. That is what makes the
    /// future `'static` despite the `&Page` parameter — and asserting it here also
    /// pins `Page: Send + Sync`, since the block holds one across its awaits.
    ///
    /// Every other test in this crate now drives this same implementation through
    /// `block_on`, so behaviour is covered by the exact-string assertions above.
    /// What none of them can see is this type, which is the entire point of the
    /// exercise. The document is dropped first to show the page stands alone.
    #[test]
    fn the_async_entry_point_yields_a_spawnable_future() {
        fn assert_send_static<F: Future + Send + 'static>(_: &F) {}

        let doc = Document::load(simple_doc("Hello")).unwrap();
        let spans_page = doc.page(0).unwrap();
        drop(doc);

        let spans = async move {
            extract_spans_with(
                NullSource {
                    payload: Vec::new(),
                },
                &spans_page,
                None,
            )
            .await
        };
        assert_send_static(&spans);

        // A source that resolves everything to null yields a page with no
        // contents, so driving this only proves the wiring is reachable.
        assert!(block_on(spans).unwrap().is_empty());
    }
}
