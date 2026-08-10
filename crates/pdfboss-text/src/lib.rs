//! Text extraction for pdfboss: font loading, encodings, ToUnicode CMaps,
//! and positional text spans.

mod cmap;
mod extract;
mod font;
mod sfnt;

use pdfboss_core::{block_on, AsyncObjectSource, Document, Immediate, Page, Result};

pub use extract::{ExtractReport, FontCache, SkipCause, SkippedText, SkippedTextKind};
pub use pdfboss_core::Point;

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
    /// Whether the font that produced this span is bold: FontDescriptor
    /// `/FontWeight` >= 600 or `/Flags` ForceBold, else a `Bold` substring
    /// in `/BaseFont` (ISO 32000-1 Table 123).
    pub bold: bool,
    /// Whether the font that produced this span is italic: FontDescriptor
    /// `/Flags` Italic or a nonzero `/ItalicAngle`, else an `Italic` or
    /// `Oblique` substring in `/BaseFont` (ISO 32000-1 Table 123).
    pub italic: bool,
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
pub fn extract_spans(doc: &Document, page: &Page) -> Result<Vec<TextSpan>> {
    block_on(extract_spans_with(Immediate(doc), page))
}

/// [`extract_spans`] against any object source, awaiting whatever I/O the
/// source needs to read the page.
///
/// This is the implementation; [`extract_spans`] is this function over
/// [`Immediate`], driven to completion on the calling thread. The two cannot
/// disagree about what a document says, because there is only one of them.
///
/// The source is taken by value and the page by reference. That combination is
/// what a consumer needs to spawn the result: the future is `Send` over a source
/// that is `Send + Sync`, and `'static` as long as the borrow of `page` is
/// created inside the consumer's own `async move` block, which owns the page.
/// See `pdfboss_core::source`'s "Signing a shared algorithm".
pub async fn extract_spans_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> Result<Vec<TextSpan>> {
    let (spans, _, _) = extract::page_spans_and_rulings_with(src, page, None).await;
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
    block_on(extract_spans_reporting_with(Immediate(doc), page))
}

/// [`extract_spans_reporting`] against any object source. Signed like
/// [`extract_spans_with`], for the same reasons.
pub async fn extract_spans_reporting_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> Result<(Vec<TextSpan>, ExtractReport)> {
    let (spans, _, report) = extract::page_spans_and_rulings_with(src, page, None).await;
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
    block_on(extract_spans_reporting_cached_with(
        Immediate(doc),
        page,
        fonts,
    ))
}

/// [`extract_spans_reporting_cached`] against any object source. Signed like
/// [`extract_spans_with`], for the same reasons.
pub async fn extract_spans_reporting_cached_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    fonts: &FontCache,
) -> Result<(Vec<TextSpan>, ExtractReport)> {
    let (spans, _, report) = extract::page_spans_and_rulings_with(src, page, Some(fonts)).await;
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
    block_on(extract_spans_and_rulings_reporting_with(
        Immediate(doc),
        page,
    ))
}

/// [`extract_spans_and_rulings_reporting`] against any object source. Signed
/// like [`extract_spans_with`], for the same reasons.
pub async fn extract_spans_and_rulings_reporting_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> Result<(Vec<TextSpan>, Vec<Ruling>, ExtractReport)> {
    let (spans, rulings, report) = extract::page_spans_and_rulings_with(src, page, None).await;
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
    block_on(extract_spans_and_rulings_reporting_cached_with(
        Immediate(doc),
        page,
        fonts,
    ))
}

/// [`extract_spans_and_rulings_reporting_cached`] against any object source.
/// Signed like [`extract_spans_with`], for the same reasons.
pub async fn extract_spans_and_rulings_reporting_cached_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    fonts: &FontCache,
) -> Result<(Vec<TextSpan>, Vec<Ruling>, ExtractReport)> {
    let (spans, rulings, report) =
        extract::page_spans_and_rulings_with(src, page, Some(fonts)).await;
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
        let (spans, report) = block_on(extract_spans_reporting_with(&counting, &page)).unwrap();
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
                &counting, &page, &fonts,
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
            )
            .await
        };
        assert_send_static(&spans);

        // A source that resolves everything to null yields a page with no
        // contents, so driving this only proves the wiring is reachable.
        assert!(block_on(spans).unwrap().is_empty());
    }
}
