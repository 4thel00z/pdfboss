//! Layout analysis and output rendering for pdfboss: turns `pdfboss-text`
//! spans into reading-order text.

mod structure;

use pdfboss_core::{block_on, AsyncObjectSource, Document, Immediate, Page, Result};

pub use pdfboss_text::{ExtractReport, SkipCause, SkippedText, SkippedTextKind, TextSpan};
pub use structure::layout;

/// Extracts the page's text with positional layout applied: spans grouped
/// into lines, lines ordered top to bottom and joined with `\n`, spaces
/// inserted at horizontal gaps.
///
/// Lenient the way rendering is: content that will not fetch, decode, or
/// parse yields no text rather than an error, so one unreadable stream
/// never costs a caller the rest of the document. Use
/// [`extract_text_reporting`] to see what (if anything) was left out.
pub fn extract_text(doc: &Document, page: &Page) -> Result<String> {
    block_on(extract_text_with(Immediate(doc), page))
}

/// [`extract_text`] against any object source, awaiting whatever I/O the
/// source needs to read the page.
///
/// This is the implementation; [`extract_text`] is this function over
/// [`Immediate`], driven to completion on the calling thread. The two cannot
/// disagree about what a document says, because there is only one of them.
///
/// The source is taken by value and the page by reference. That combination is
/// what a consumer needs to spawn the result: the future is `Send` over a source
/// that is `Send + Sync`, and `'static` as long as the borrow of `page` is
/// created inside the consumer's own `async move` block, which owns the page.
/// See `pdfboss_core::source`'s "Signing a shared algorithm".
pub async fn extract_text_with<S: AsyncObjectSource>(src: S, page: &Page) -> Result<String> {
    let (text, _) = extract_text_reporting_with(src, page).await?;
    Ok(text)
}

/// [`extract_text`] with the report of what could not be read: an
/// [`ExtractReport`] whose entries name each skipped stream and why —
/// unsupported filters (the passthrough image codecs included), undecodable
/// bytes, unparseable content, missing resources, exhausted form limits.
/// An empty text with an empty report really is an empty page.
pub fn extract_text_reporting(doc: &Document, page: &Page) -> Result<(String, ExtractReport)> {
    block_on(extract_text_reporting_with(Immediate(doc), page))
}

/// [`extract_text_reporting`] against any object source. Signed like
/// [`extract_text_with`], for the same reasons.
pub async fn extract_text_reporting_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> Result<(String, ExtractReport)> {
    let (spans, report) = pdfboss_text::extract_spans_reporting_with(src, page).await?;
    Ok((structure::layout(&spans), report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{resolve_with, BoxFuture, ObjRef, Object, Stream};
    use pdfboss_testkit::{multi_page_doc, simple_doc, PdfBuilder};
    use std::future::Future;

    fn page_text(doc: &Document, index: usize) -> String {
        let page = doc.page(index).unwrap();
        extract_text(doc, &page).unwrap()
    }

    #[test]
    fn simple_doc_exact_text() {
        let doc = Document::load(simple_doc("Hello, world!")).unwrap();
        assert_eq!(page_text(&doc, 0), "Hello, world!");
    }

    #[test]
    fn multi_page_doc_per_page() {
        let doc = Document::load(multi_page_doc(&["Page one", "Page two", "Page three"])).unwrap();
        assert_eq!(doc.page_count(), 3);
        assert_eq!(page_text(&doc, 0), "Page one");
        assert_eq!(page_text(&doc, 1), "Page two");
        assert_eq!(page_text(&doc, 2), "Page three");
    }

    #[test]
    fn differences_remap_in_extraction() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (AB) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Custom \
             /Encoding << /BaseEncoding /WinAnsiEncoding \
             /Differences [65 /alpha] >> >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "\u{3B1}B");
    }

    #[test]
    fn type0_font_with_tounicode_stream() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td <00010001> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
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
              1 beginbfchar <0001> <03A9> endbfchar",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "\u{3A9}\u{3A9}");
    }

    #[test]
    fn form_xobject_recursion() {
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
        assert_eq!(page_text(&doc, 0), "out\nin");
    }

    /// A form XObject that carries its own `/Resources` **without** a `/Font`
    /// entry must still find the page's font. Resource lookup is a chain,
    /// innermost first with a per-name fallback (ISO 32000 §8.10.2 and
    /// §7.8.3) — not replace-or-inherit.
    ///
    /// `/Differences` is what makes the failure visible rather than silent:
    /// through the page's `/F1`, byte 65 decodes to alpha; through the
    /// fallback font it stays `"A"`. Without the chain the text is still
    /// extracted, just decoded with the wrong font, which is why no existing
    /// test caught this.
    #[test]
    fn form_with_partial_resources_still_sees_the_page_font() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Custom \
             /Encoding << /BaseEncoding /WinAnsiEncoding \
             /Differences [65 /alpha] >> >>",
        );
        // Own /Resources present, but it defines no /Font at all.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Resources << /ProcSet [/PDF /Text] >>",
            b"BT /F1 12 Tf 72 720 Td (A) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "\u{3B1}");
    }

    /// Hex-encodes `data` for an `/ASCIIHexDecode` stream — the benign
    /// trailing filter of the pass-through tests: refusal must be about the
    /// image codecs, not about `/Filter` being present at all.
    fn hex(data: &[u8]) -> Vec<u8> {
        data.iter()
            .flat_map(|b| format!("{b:02X}").into_bytes())
            .chain(*b">")
            .collect()
    }

    /// One page whose `/Contents` (object 4) carries `stream_dict` around
    /// `content`, with `/F1` a WinAnsi Helvetica.
    fn contents_doc(stream_dict: &str, content: &[u8]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, stream_dict, content);
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        Document::load(b.build(1)).unwrap()
    }

    /// A page `/Contents` whose trailing `/Filter` is an image codec is
    /// refused, and the refusal is a report entry, not an error: the page
    /// yields no text instead of costing a document-level caller every
    /// other page. The bytes are deliberately valid operators to prove the
    /// refusal happens on the label.
    #[test]
    fn image_codec_page_contents_yield_no_text_and_one_report_entry() {
        let doc = contents_doc(
            "/Filter /JPXDecode",
            b"BT /F1 12 Tf 72 720 Td (ghost) Tj ET",
        );
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "", "the passthrough bytes must not be parsed");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::PageContents,
                cause: SkipCause::UnsupportedFilter("JPXDecode".to_string()),
            }],
        );
        // The plain entry point is the same leniency without the report.
        assert_eq!(extract_text(&doc, &page).unwrap(), "");
    }

    /// The inverse: a benign trailing filter the decoder can run must keep
    /// decoding. Over-refusal here would silently drop the text of every
    /// compressed page while the suite stayed green.
    #[test]
    fn a_filtered_page_contents_still_extracts() {
        let doc = contents_doc(
            "/Filter /ASCIIHexDecode",
            &hex(b"BT /F1 12 Tf 72 720 Td (plain sight) Tj ET"),
        );
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "plain sight");
        assert!(report.is_complete(), "nothing was skipped: {report:?}");
    }

    /// A form XObject whose trailing `/Filter` is an image codec is refused
    /// with a report entry — the same accountable skip rendering records —
    /// while the rest of the page still extracts. Before the report channel
    /// existed this text vanished with zero signal.
    #[test]
    fn image_codec_form_content_is_refused_and_reported() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (kept) Tj ET /Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Filter /DCTDecode",
            b"BT /F1 12 Tf 72 700 Td (ghost) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "kept", "the page's own text survives the refusal");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::Form,
                cause: SkipCause::UnsupportedFilter("DCTDecode".to_string()),
            }],
        );
    }

    /// The form-level inverse: a benign trailing filter on a form decodes
    /// and its text extracts, with a complete report.
    #[test]
    fn a_filtered_form_still_extracts() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Filter /ASCIIHexDecode",
            &hex(b"BT /F1 12 Tf 72 720 Td (decoded) Tj ET"),
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "decoded");
        assert!(report.is_complete(), "nothing was skipped: {report:?}");
    }

    /// A self-invoking form recurses to the depth cap, and the cap is a
    /// report entry rather than a silent stop.
    #[test]
    fn exhausted_form_depth_is_reported() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        // No own /Resources: the page's names itself, so each level invokes
        // the next until the depth cap bites at the innermost one.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792]",
            b"BT /F1 12 Tf 72 720 Td (x) Tj ET /Fx Do",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert!(!text.is_empty(), "the levels above the cap still extract");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::Form,
                cause: SkipCause::LimitExceeded,
            }],
        );
    }

    /// A `Do` whose name resolves to nothing usable is reported: whether it
    /// held text cannot be known, so a complete report must not pretend so.
    #[test]
    fn a_missing_xobject_is_reported() {
        let doc = contents_doc("", b"BT /F1 12 Tf 72 720 Td (here) Tj ET /Nope Do");
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "here");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::XObject,
                cause: SkipCause::Missing,
            }],
        );
    }

    /// A conforming file may make any dictionary value indirect (ISO 32000-1
    /// 7.3.8.1), `/Subtype` included. The form dispatch resolves it rather
    /// than requiring a direct name — a form declared through a reference
    /// used to be dropped as "not a form", burning its invocation-budget
    /// slot and silently losing its whole text subtree.
    #[test]
    fn a_form_whose_subtype_is_indirect_still_extracts() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype 8 0 R /BBox [0 0 612 792]",
            b"BT /F1 12 Tf 72 720 Td (via ref) Tj ET",
        );
        b.object(8, "/Form");
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "via ref");
    }

    /// The same chain rule for a nested form: an inner form named only in the
    /// page's `/XObject` must be reachable from a form that has its own
    /// `/Resources` without an `/XObject` entry.
    #[test]
    fn form_with_partial_resources_still_sees_the_page_xobject() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> \
             /XObject << /Outer 6 0 R /Inner 7 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Outer Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        // Outer has its own /Resources naming neither /Inner nor /Font.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Resources << /ProcSet [/PDF /Text] >>",
            b"/Inner Do",
        );
        b.stream(
            7,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792]",
            b"BT /F1 12 Tf 72 720 Td (deep) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "deep");
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
    /// `extract_text_with` takes is created inside it. That is what makes the
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
        let text_page = doc.page(0).unwrap();
        drop(doc);

        let text = async move {
            extract_text_with(
                NullSource {
                    payload: Vec::new(),
                },
                &text_page,
            )
            .await
        };
        assert_send_static(&text);

        // A source that resolves everything to null yields a page with no
        // contents, so driving this only proves the wiring is reachable.
        assert_eq!(block_on(text).unwrap(), "");
    }

    #[test]
    fn committed_fixture_files() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures");
        let hello = std::fs::read(format!("{dir}/hello.pdf")).unwrap();
        let doc = Document::load(hello).unwrap();
        assert_eq!(page_text(&doc, 0), "Hello, world!");

        let three = std::fs::read(format!("{dir}/three-pages.pdf")).unwrap();
        let doc = Document::load(three).unwrap();
        assert_eq!(doc.page_count(), 3);
        assert_eq!(page_text(&doc, 0), "Page one");
        assert_eq!(page_text(&doc, 1), "Page two");
        assert_eq!(page_text(&doc, 2), "Page three");

        let xs = std::fs::read(format!("{dir}/xref-stream.pdf")).unwrap();
        let doc = Document::load(xs).unwrap();
        assert_eq!(page_text(&doc, 0), "Hello, world!");
    }
}
