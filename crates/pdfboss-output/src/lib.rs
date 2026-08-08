//! Layout analysis and output rendering for pdfboss: turns `pdfboss-text`
//! spans into a structured layout IR, and the IR into a document.

mod ir;
mod markdown;
mod output;
mod structure;

use pdfboss_core::{block_on, AsyncObjectSource, Document, Immediate, Page, Result};

pub use ir::{BBox, Block, Cell, Inline, Line, ListItem, Marker, PageLayout, Role};
pub use markdown::Markdown;
pub use output::{Output, Text};
pub use pdfboss_text::{ExtractReport, SkipCause, SkippedText, SkippedTextKind, TextSpan};
pub use structure::{document_layout, layout, page_layout};

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
    Ok((Text.render(&[page_layout(&spans)]), report))
}

/// Extracts the whole document as Markdown: ATX headings, paragraphs, and
/// emphasis over the same positional layout [`extract_text`] renders flat.
///
/// Heading sizes are ranked against every page at once, so a title page or
/// a chapter opener — all of it larger than body text — is read as headings
/// rather than as its own idea of body size.
///
/// Lenient like [`extract_text`]: unreadable content costs its own text and
/// nothing else. Use [`extract_markdown_reporting`] to see what was left
/// out.
pub fn extract_markdown(doc: &Document) -> Result<String> {
    let (markdown, _) = extract_markdown_reporting(doc)?;
    Ok(markdown)
}

/// [`extract_markdown`] with one [`ExtractReport`] per page, in page order.
pub fn extract_markdown_reporting(doc: &Document) -> Result<(String, Vec<ExtractReport>)> {
    let per_page = pdfboss_core::map_pages(doc, pdfboss_text::extract_spans_reporting);
    let mut pages = Vec::with_capacity(per_page.len());
    let mut reports = Vec::with_capacity(per_page.len());
    for outcome in per_page {
        let (spans, report) = outcome?;
        pages.push(spans);
        reports.push(report);
    }
    Ok((Markdown.render(&document_layout(&pages)), reports))
}

/// One page as Markdown, ranking heading sizes against that page alone.
/// [`extract_markdown`] is the better answer whenever the document is at
/// hand — a page whose text is all one size has no heading to find.
pub fn extract_page_markdown(doc: &Document, page: &Page) -> Result<String> {
    block_on(extract_page_markdown_with(Immediate(doc), page))
}

/// [`extract_page_markdown`] against any object source. Signed like
/// [`extract_text_with`], for the same reasons.
///
/// There is no document-level `_with`: an asynchronous caller collects each
/// page's spans with `pdfboss_text::extract_spans_reporting_with` and then
/// calls the pure [`document_layout`] and [`Markdown`], which is the same
/// document-wide ranking without a second I/O path to keep in step.
pub async fn extract_page_markdown_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> Result<String> {
    let (spans, _) = pdfboss_text::extract_spans_reporting_with(src, page).await?;
    Ok(Markdown.render(&[page_layout(&spans)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{resolve_with, BoxFuture, ObjRef, Object, Stream};
    use pdfboss_testkit::{doc_with_graphics, multi_page_doc, simple_doc, PdfBuilder};
    use std::future::Future;

    fn page_text(doc: &Document, index: usize) -> String {
        let page = doc.page(index).unwrap();
        extract_text(doc, &page).unwrap()
    }

    /// The Text adapter over the IR must reproduce the pre-IR string builder
    /// exactly — the local form of the corpus parity gate.
    /// [`structure::layout_reference`] is that builder, kept as the oracle.
    #[test]
    fn text_adapter_matches_layout_on_fixtures() {
        let mut headings = 0usize;
        let mut splits = 0usize;
        for content in structure::tests::fixture_contents() {
            let doc = Document::load(doc_with_graphics(&content)).unwrap();
            let page = doc.page(0).unwrap();
            let (spans, report) = pdfboss_text::extract_spans_reporting(&doc, &page).unwrap();
            assert!(report.is_complete(), "unexpected skips: {report:?}");
            let layout = page_layout(&spans);
            headings += layout
                .blocks
                .iter()
                .filter(|block| matches!(block, Block::Heading { .. }))
                .count();
            let paragraphs = layout
                .blocks
                .iter()
                .filter(|block| matches!(block, Block::Paragraph { .. }))
                .count();
            splits += usize::from(paragraphs > 1);
            let via_ir = Text.render(&[layout]);
            assert_eq!(
                via_ir,
                structure::layout_reference(&spans),
                "content: {content}"
            );
        }
        // A fixture set that classifies nothing would pass this test without
        // ever reaching the code it guards.
        assert!(headings > 0, "no fixture produced a heading block");
        assert!(splits > 0, "no fixture split into several paragraphs");
    }

    /// Markdown of a one-page document with `content` as its raw content
    /// stream, through document-level size statistics.
    fn markdown_of(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, report) = pdfboss_text::extract_spans_reporting(&doc, &page).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        Markdown.render(&document_layout(&[spans]))
    }

    /// [`markdown_of`] on a page whose resources carry `/F1` Helvetica and
    /// `/F2` Helvetica-Bold, so a span's boldness comes from a real font.
    fn markdown_of_two_fonts(content: &str) -> String {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", content.as_bytes());
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, report) = pdfboss_text::extract_spans_reporting(&doc, &page).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        Markdown.render(&document_layout(&[spans]))
    }

    /// Sizes rank into levels; body text stays a paragraph. 24pt > 16pt > 12pt
    /// body: `#` for 24, `##` for 16.
    #[test]
    fn heading_levels_by_size_rank() {
        let content = "BT /F1 24 Tf 72 740 Td (Title) Tj \
                       /F1 16 Tf 0 -40 Td (Section) Tj \
                       /F1 12 Tf 0 -30 Td (Body text long enough to look like body.) Tj \
                       0 -14 Td (More body keeps twelve the dominant size.) Tj \
                       0 -14 Td (And a third line for good measure.) Tj ET";
        let md = markdown_of(content);
        assert!(md.contains("# Title\n"), "md: {md}");
        assert!(md.contains("## Section\n"), "md: {md}");
        assert!(!md.contains("# Body"), "md: {md}");
    }

    /// Sizes past the sixth ladder rank clamp to `######` instead of falling
    /// out of the ladder: the buckets nearest body size are the real section
    /// headings, and one stray oversized logo must not evict them.
    #[test]
    fn ranks_past_six_clamp_to_level_six() {
        let heads = [36, 28, 24, 20, 18, 16, 14, 12, 11]
            .iter()
            .enumerate()
            .map(|(index, size)| {
                let y = 750 - 50 * index;
                format!("BT /F1 {size} Tf 72 {y} Td (Head {size}) Tj ET ")
            })
            .collect::<String>();
        let body = (0..3)
            .map(|index| {
                let y = 260 - 14 * index;
                format!(
                    "BT /F1 10 Tf 72 {y} Td (Body line {index} is long enough to be body.) Tj ET "
                )
            })
            .collect::<String>();
        let md = markdown_of(&format!("{heads}{body}"));
        assert!(md.starts_with("# Head 36"), "md: {md}");
        assert!(md.contains("###### Head 16"), "md: {md}");
        assert!(md.contains("###### Head 14"), "md: {md}");
        assert!(md.contains("###### Head 12"), "md: {md}");
        assert!(md.contains("###### Head 11"), "md: {md}");
    }

    /// A whitespace-only line at heading size is still classified as a
    /// heading; Markdown must not emit a bare `#` for it.
    #[test]
    fn blank_heading_line_emits_nothing() {
        let md = markdown_of(
            "BT /F1 24 Tf 72 740 Td (   ) Tj \
             /F1 12 Tf 0 -40 Td (Body line one is long enough to be body.) Tj \
             0 -14 Td (Body line two keeps twelve the dominant size.) Tj \
             0 -14 Td (And a third body line seals it.) Tj ET",
        );
        assert!(!md.contains('#'), "md: {md:?}");
    }

    /// Emphasis wraps maximal same-style runs, with the spaces left outside
    /// the markers.
    #[test]
    fn bold_run_renders_as_strong() {
        let md = markdown_of_two_fonts(
            "BT /F1 12 Tf 72 720 Td (plain ) Tj /F2 12 Tf (loud) Tj /F1 12 Tf ( tail) Tj \
             0 -14 Td (body body body body) Tj 0 -14 Td (body body body body) Tj ET",
        );
        assert!(md.contains("plain **loud** tail"), "md: {md}");
    }

    /// A lone page of huge text must not become all headings under per-page
    /// stats when the document knows better.
    #[test]
    fn document_stats_beat_page_stats() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 5 0 R >>",
        );
        b.object(
            4,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>",
        );
        b.stream(
            5,
            "",
            b"BT /F1 12 Tf 72 720 Td (Body line one is long enough.) Tj \
              0 -14 Td (Body line two keeps twelve dominant.) Tj \
              0 -14 Td (Body line three seals it.) Tj ET",
        );
        b.stream(6, "", b"BT /F1 24 Tf 72 720 Td (Chapter Two) Tj ET");
        b.object(
            7,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let pages: Vec<Vec<TextSpan>> = (0..2)
            .map(|i| {
                let page = doc.page(i).unwrap();
                pdfboss_text::extract_spans_reporting(&doc, &page)
                    .unwrap()
                    .0
            })
            .collect();
        let md = Markdown.render(&document_layout(&pages));
        assert!(
            md.contains("# Chapter Two"),
            "doc stats make it a heading: {md}"
        );
        let alone = Markdown.render(&[page_layout(&pages[1])]);
        assert!(
            !alone.contains("# "),
            "page stats alone see 24pt as body: {alone}"
        );
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
