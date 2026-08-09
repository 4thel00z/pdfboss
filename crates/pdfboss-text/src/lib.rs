//! Text extraction for pdfboss: font loading, encodings, ToUnicode CMaps,
//! and positional text spans.

mod cmap;
mod extract;
mod font;
mod sfnt;

use pdfboss_core::{block_on, AsyncObjectSource, Document, Immediate, Page, Result};

pub use extract::{ExtractReport, SkipCause, SkippedText, SkippedTextKind};

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
    let (spans, _) = extract::page_spans_with(src, page).await;
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
    let (spans, report) = extract::page_spans_with(src, page).await;
    Ok((spans, report))
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
