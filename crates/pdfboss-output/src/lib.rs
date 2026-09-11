//! Layout analysis and output rendering for pdfboss: turns `pdfboss-text`
//! spans into a structured layout IR, and the IR into a document.

mod ir;
mod markdown;
mod output;
mod structure;

use pdfboss_core::{AsyncObjectSource, Document, OcState, Page, Result, StructureTree};

pub use ir::{BBox, Block, Cell, Inline, Line, ListItem, Marker, PageLayout, Role};
pub use markdown::Markdown;
pub use output::{Output, Text};
pub use pdfboss_text::{
    ExtractReport, FontCache, MarkedContentId, ReadingOrder, Ruling, SkipCause, SkippedText,
    SkippedTextKind, TextSpan,
};
pub use structure::{
    document_layout, document_layout_with_rulings, layout, page_layout, page_layout_with_rulings,
};

/// What extraction keeps beyond the visible page.
///
/// By default the `extract_*` entries read what a viewer shows: spans and
/// rulings lying entirely outside the page's crop box are dropped before
/// layout. A page cropped out of a larger document often keeps its
/// neighbors' content in the stream — pasteboard text no viewer renders —
/// and `invisible_text: true` extracts it too.
///
/// Text drawn with render mode 3 (an OCR layer over a scan) is on the page,
/// selectable in a viewer, and always extracted; this option is only about
/// content outside the page box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextOptions {
    /// Keep content outside the page's crop box.
    pub invisible_text: bool,
}

/// The overlap window of `page`'s crop box, one point of tolerance to each
/// side so a glyph overhanging the margin stays on the page.
fn page_window(page: &Page) -> (f32, f32, f32, f32) {
    const TOLERANCE: f32 = 1.0;
    let crop = page.crop_box;
    (
        crop.x0.min(crop.x1) - TOLERANCE,
        crop.x0.max(crop.x1) + TOLERANCE,
        crop.y0.min(crop.y1) - TOLERANCE,
        crop.y0.max(crop.y1) + TOLERANCE,
    )
}

/// Drops the spans that lie entirely outside `page`'s crop box (overlap
/// test, a point of tolerance). This is what the `extract_*` entries apply
/// by default; a caller composing an extraction from raw spans — the
/// document-level asynchronous Markdown path — applies it to match them.
pub fn retain_spans_on_page(spans: &mut Vec<TextSpan>, page: &Page) {
    let (x0, x1, y0, y1) = page_window(page);
    spans.retain(|s| {
        s.bbox.x1.max(s.bbox.x0) >= x0
            && s.bbox.x0.min(s.bbox.x1) <= x1
            && s.bbox.y1.max(s.bbox.y0) >= y0
            && s.bbox.y0.min(s.bbox.y1) <= y1
    });
}

/// [`retain_spans_on_page`] for rulings: drops the segments that lie
/// entirely outside `page`'s crop box, so an off-page grid cannot become a
/// table.
pub fn retain_rulings_on_page(rulings: &mut Vec<Ruling>, page: &Page) {
    let (x0, x1, y0, y1) = page_window(page);
    rulings.retain(|r| {
        r.start.x.max(r.end.x) >= x0
            && r.start.x.min(r.end.x) <= x1
            && r.start.y.max(r.end.y) >= y0
            && r.start.y.min(r.end.y) <= y1
    });
}

/// Extracts the page's text with layout applied: spans grouped into lines,
/// lines in the [`ReadingOrder`] given and joined with `\n`, spaces
/// inserted at horizontal gaps. [`ReadingOrder::Content`] is the default
/// order; [`ReadingOrder::StructureTree`] reads a tagged page as its
/// structure tree does and every other page in content order;
/// [`ReadingOrder::Geometric`] reads by position alone.
///
/// Lenient the way rendering is: content that will not fetch, decode, or
/// parse yields no text rather than an error, so one unreadable stream
/// never costs a caller the rest of the document. Use
/// [`extract_text_reporting`] to see what (if anything) was left out.
///
/// Optional-content layers the document's default configuration turns off
/// are excluded, exactly as `pdfboss_text`'s document-level entries exclude
/// them; the source-generic `_with` twins have no document to read that
/// configuration from and extract every layer.
pub fn extract_text(doc: &Document, page: &Page, order: ReadingOrder) -> Result<String> {
    let (text, _) = extract_text_reporting(doc, page, order)?;
    Ok(text)
}

/// [`extract_text`] against any object source, awaiting whatever I/O the
/// source needs to read the page — the same span extraction and layout.
/// `oc` is the document's optional-content visibility (the async document's
/// `oc_state()`); `None` extracts every layer. `structure` is its structure
/// tree (the async document's `structure_tree()`), read only under
/// [`ReadingOrder::StructureTree`]; `None` there reads content order.
///
/// The source is taken by value and the page by reference. That combination is
/// what a consumer needs to spawn the result: the future is `Send` over a source
/// that is `Send + Sync`, and `'static` as long as the borrow of `page` is
/// created inside the consumer's own `async move` block, which owns the page.
/// See `pdfboss_core::source`'s "Signing a shared algorithm".
pub async fn extract_text_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    oc: Option<&OcState>,
    structure: Option<&StructureTree>,
    order: ReadingOrder,
) -> Result<String> {
    let (text, _) = extract_text_reporting_with(src, page, oc, structure, order).await?;
    Ok(text)
}

/// [`extract_text`] with the report of what could not be read: an
/// [`ExtractReport`] whose entries name each skipped stream and why —
/// unsupported filters (the passthrough image codecs included), undecodable
/// bytes, unparseable content, missing resources, exhausted form limits.
/// An empty text with an empty report is a page a viewer shows as empty —
/// either truly empty, or holding only off-page content, which
/// [`TextOptions::invisible_text`] recovers.
pub fn extract_text_reporting(
    doc: &Document,
    page: &Page,
    order: ReadingOrder,
) -> Result<(String, ExtractReport)> {
    extract_text_reporting_opts(doc, page, order, TextOptions::default())
}

/// [`extract_text_reporting`] with [`TextOptions`]: `invisible_text` keeps
/// the content outside the page box that the default drops.
pub fn extract_text_reporting_opts(
    doc: &Document,
    page: &Page,
    order: ReadingOrder,
    opts: TextOptions,
) -> Result<(String, ExtractReport)> {
    let (mut spans, report) = pdfboss_text::extract_spans_reporting(doc, page, order)?;
    if !opts.invisible_text {
        retain_spans_on_page(&mut spans, page);
    }
    Ok((Text.render(&[page_layout(&spans, report.order)]), report))
}

/// [`extract_text_reporting`] against any object source. Signed like
/// [`extract_text_with`], for the same reasons — `oc` gating included.
pub async fn extract_text_reporting_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    oc: Option<&OcState>,
    structure: Option<&StructureTree>,
    order: ReadingOrder,
) -> Result<(String, ExtractReport)> {
    extract_text_reporting_with_opts(src, page, oc, structure, order, TextOptions::default()).await
}

/// [`extract_text_reporting_with`] with [`TextOptions`]: `invisible_text`
/// keeps the content outside the page box that the default drops.
pub async fn extract_text_reporting_with_opts<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    oc: Option<&OcState>,
    structure: Option<&StructureTree>,
    order: ReadingOrder,
    opts: TextOptions,
) -> Result<(String, ExtractReport)> {
    let (mut spans, report) =
        pdfboss_text::extract_spans_reporting_with(src, page, oc, structure, order).await?;
    if !opts.invisible_text {
        retain_spans_on_page(&mut spans, page);
    }
    Ok((Text.render(&[page_layout(&spans, report.order)]), report))
}

/// [`extract_text_reporting`] with fonts cached across pages: a caller
/// walking a whole document — `pdfboss_core::map_pages` included — passes one
/// [`FontCache`] to every page and each font loads once for the document.
/// The text is identical to the uncached call's, page for page.
///
/// There is no `_with` twin: an asynchronous caller composes
/// `pdfboss_text::extract_spans_reporting_cached_with` with the pure
/// [`page_layout`] and [`Text`], exactly as this function does.
pub fn extract_text_reporting_cached(
    doc: &Document,
    page: &Page,
    fonts: &FontCache,
    order: ReadingOrder,
) -> Result<(String, ExtractReport)> {
    extract_text_reporting_cached_opts(doc, page, fonts, order, TextOptions::default())
}

/// [`extract_text_reporting_cached`] with [`TextOptions`]: `invisible_text`
/// keeps the content outside the page box that the default drops.
pub fn extract_text_reporting_cached_opts(
    doc: &Document,
    page: &Page,
    fonts: &FontCache,
    order: ReadingOrder,
    opts: TextOptions,
) -> Result<(String, ExtractReport)> {
    let (mut spans, report) =
        pdfboss_text::extract_spans_reporting_cached(doc, page, fonts, order)?;
    if !opts.invisible_text {
        retain_spans_on_page(&mut spans, page);
    }
    Ok((Text.render(&[page_layout(&spans, report.order)]), report))
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
pub fn extract_markdown(doc: &Document, order: ReadingOrder) -> Result<String> {
    let (markdown, _) = extract_markdown_reporting(doc, order)?;
    Ok(markdown)
}

/// [`extract_markdown`] with [`TextOptions`]: `invisible_text` keeps the
/// content outside each page's box that the default drops.
pub fn extract_markdown_opts(
    doc: &Document,
    order: ReadingOrder,
    opts: TextOptions,
) -> Result<String> {
    let (markdown, _) = extract_markdown_reporting_opts(doc, order, opts)?;
    Ok(markdown)
}

/// [`extract_markdown`] with one [`ExtractReport`] per page, in page order.
///
/// Each page's rulings ride along with its spans: a table whose structure is
/// drawn as borders is read from them ahead of lane occupancy.
pub fn extract_markdown_reporting(
    doc: &Document,
    order: ReadingOrder,
) -> Result<(String, Vec<ExtractReport>)> {
    extract_markdown_reporting_opts(doc, order, TextOptions::default())
}

/// [`extract_markdown_reporting`] with [`TextOptions`]: `invisible_text`
/// keeps the content outside each page's box that the default drops.
pub fn extract_markdown_reporting_opts(
    doc: &Document,
    order: ReadingOrder,
    opts: TextOptions,
) -> Result<(String, Vec<ExtractReport>)> {
    let fonts = FontCache::default();
    let per_page = pdfboss_core::map_pages(doc, |doc: &Document, page: &Page| {
        let (mut spans, mut rulings, report) =
            pdfboss_text::extract_spans_and_rulings_reporting_cached(doc, page, &fonts, order)?;
        if !opts.invisible_text {
            retain_spans_on_page(&mut spans, page);
            retain_rulings_on_page(&mut rulings, page);
        }
        Ok((spans, rulings, report))
    });
    let mut pages = Vec::with_capacity(per_page.len());
    let mut reports = Vec::with_capacity(per_page.len());
    for outcome in per_page {
        let (spans, rulings, report) = outcome?;
        pages.push((spans, rulings, report.order));
        reports.push(report);
    }
    Ok((
        Markdown.render(&document_layout_with_rulings(&pages)),
        reports,
    ))
}

/// One page as Markdown, ranking heading sizes against that page alone.
/// [`extract_markdown`] is the better answer whenever the document is at
/// hand — a page whose text is all one size has no heading to find.
pub fn extract_page_markdown(doc: &Document, page: &Page, order: ReadingOrder) -> Result<String> {
    extract_page_markdown_opts(doc, page, order, TextOptions::default())
}

/// [`extract_page_markdown`] with [`TextOptions`]: `invisible_text` keeps
/// the content outside the page box that the default drops.
pub fn extract_page_markdown_opts(
    doc: &Document,
    page: &Page,
    order: ReadingOrder,
    opts: TextOptions,
) -> Result<String> {
    let (mut spans, mut rulings, report) =
        pdfboss_text::extract_spans_and_rulings_reporting(doc, page, order)?;
    if !opts.invisible_text {
        retain_spans_on_page(&mut spans, page);
        retain_rulings_on_page(&mut rulings, page);
    }
    Ok(Markdown.render(&[page_layout_with_rulings(&spans, &rulings, report.order)]))
}

/// [`extract_page_markdown`] against any object source. Signed like
/// [`extract_text_with`], for the same reasons — `oc` gating included.
///
/// There is no document-level `_with`: an asynchronous caller collects each
/// page's spans and rulings with
/// `pdfboss_text::extract_spans_and_rulings_reporting_with` and then calls
/// the pure [`document_layout_with_rulings`] and [`Markdown`], which is the
/// same document-wide ranking without a second I/O path to keep in step.
pub async fn extract_page_markdown_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    oc: Option<&OcState>,
    structure: Option<&StructureTree>,
    order: ReadingOrder,
) -> Result<String> {
    extract_page_markdown_with_opts(src, page, oc, structure, order, TextOptions::default()).await
}

/// [`extract_page_markdown_with`] with [`TextOptions`]: `invisible_text`
/// keeps the content outside the page box that the default drops.
pub async fn extract_page_markdown_with_opts<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    oc: Option<&OcState>,
    structure: Option<&StructureTree>,
    order: ReadingOrder,
    opts: TextOptions,
) -> Result<String> {
    let (mut spans, mut rulings, report) =
        pdfboss_text::extract_spans_and_rulings_reporting_with(src, page, oc, structure, order)
            .await?;
    if !opts.invisible_text {
        retain_spans_on_page(&mut spans, page);
        retain_rulings_on_page(&mut rulings, page);
    }
    Ok(Markdown.render(&[page_layout_with_rulings(&spans, &rulings, report.order)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{block_on, resolve_with, BoxFuture, ObjRef, Object, Stream};
    use pdfboss_testkit::{
        doc_with_graphics, multi_page_doc, simple_doc, tagged_two_column_doc, PdfBuilder,
    };
    use std::future::Future;

    fn page_text(doc: &Document, index: usize) -> String {
        let page = doc.page(index).unwrap();
        extract_text(doc, &page, ReadingOrder::Content).unwrap()
    }

    /// The tagged two-column fixture read three ways: the stream as
    /// written, the page by position, the page as its structure tree says.
    // Covers ISO 32000-1 §14.8.2.
    #[test]
    fn three_reading_orders_read_the_tagged_page_three_ways() {
        let doc = Document::load(tagged_two_column_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let text = |order: ReadingOrder| extract_text(&doc, &page, order).unwrap();
        assert_eq!(text(ReadingOrder::Content), "L3 R3\nL4 R4\nL1 R1\nL2 R2");
        assert_eq!(text(ReadingOrder::Geometric), "L1 R1\nL2 R2\nL3 R3\nL4 R4");
        assert_eq!(
            text(ReadingOrder::StructureTree),
            "L1\nL2\nL3\nL4\nR1\nR2\nR3\nR4"
        );
    }

    /// The `L`/`R` labels of a markdown rendering, in the order written.
    fn labels(md: &str) -> Vec<&str> {
        md.split_whitespace()
            .filter(|token| token.starts_with('L') || token.starts_with('R'))
            .collect()
    }

    /// Markdown reads by the same order as text, page by page and
    /// document-wide.
    // Covers ISO 32000-1 §14.8.2.3.
    #[test]
    fn markdown_follows_the_reading_order() {
        let doc = Document::load(tagged_two_column_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let tree = ["L1", "L2", "L3", "L4", "R1", "R2", "R3", "R4"];
        let by_page = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(labels(&by_page), tree, "page md: {by_page}");
        let whole = extract_markdown(&doc, ReadingOrder::StructureTree).unwrap();
        assert_eq!(labels(&whole), tree, "document md: {whole}");
        let geometric = extract_markdown(&doc, ReadingOrder::Geometric).unwrap();
        assert_eq!(
            labels(&geometric),
            ["L1", "R1", "L2", "R2", "L3", "R3", "L4", "R4"],
            "geometric md: {geometric}"
        );
    }

    /// One tagged page whose every block the layout heuristics would read
    /// differently: an H1 at body size and weight, two paragraphs a line
    /// apart, a numbered list whose labels share their items' lines, and a
    /// two-row table with no rulings.
    fn tagged_blocks_doc() -> Vec<u8> {
        fn element(b: &mut PdfBuilder, num: u32, s: &str, parent: u32, kids: &str) {
            b.object(
                num,
                &format!("<< /Type /StructElem /S /{s} /P {parent} 0 R /Pg 3 0 R /K {kids} >>"),
            );
        }
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf \
              /H1 << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (Title) Tj EMC \
              /P << /MCID 1 >> BDC 1 0 0 1 72 680 Tm (First) Tj EMC \
              /P << /MCID 2 >> BDC 1 0 0 1 72 666 Tm (Second) Tj EMC \
              /Lbl << /MCID 3 >> BDC 1 0 0 1 72 640 Tm (1.) Tj EMC \
              /LBody << /MCID 4 >> BDC 1 0 0 1 90 640 Tm (One) Tj EMC \
              /Lbl << /MCID 5 >> BDC 1 0 0 1 72 626 Tm (2.) Tj EMC \
              /LBody << /MCID 6 >> BDC 1 0 0 1 90 626 Tm (Two) Tj EMC \
              /TD << /MCID 7 >> BDC 1 0 0 1 72 600 Tm (a) Tj EMC \
              /TD << /MCID 8 >> BDC 1 0 0 1 200 600 Tm (b) Tj EMC \
              /TD << /MCID 9 >> BDC 1 0 0 1 72 586 Tm (c) Tj EMC \
              /TD << /MCID 10 >> BDC 1 0 0 1 200 586 Tm (d) Tj EMC ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            10,
            "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
        );
        b.object(
            11,
            "<< /Type /StructElem /S /Document /P 10 0 R \
             /K [13 0 R 14 0 R 15 0 R 16 0 R 20 0 R] >>",
        );
        b.object(
            12,
            "<< /Nums [0 [13 0 R 14 0 R 15 0 R 30 0 R 31 0 R 32 0 R 33 0 R \
             23 0 R 24 0 R 25 0 R 26 0 R]] >>",
        );
        element(&mut b, 13, "H1", 11, "[0]");
        element(&mut b, 14, "P", 11, "[1]");
        element(&mut b, 15, "P", 11, "[2]");
        element(&mut b, 16, "L", 11, "[17 0 R 18 0 R]");
        element(&mut b, 17, "LI", 16, "[30 0 R 31 0 R]");
        element(&mut b, 30, "Lbl", 17, "[3]");
        element(&mut b, 31, "LBody", 17, "[4]");
        element(&mut b, 18, "LI", 16, "[32 0 R 33 0 R]");
        element(&mut b, 32, "Lbl", 18, "[5]");
        element(&mut b, 33, "LBody", 18, "[6]");
        element(&mut b, 20, "Table", 11, "[21 0 R 22 0 R]");
        element(&mut b, 21, "TR", 20, "[23 0 R 24 0 R]");
        element(&mut b, 23, "TD", 21, "[7]");
        element(&mut b, 24, "TD", 21, "[8]");
        element(&mut b, 22, "TR", 20, "[25 0 R 26 0 R]");
        element(&mut b, 25, "TD", 22, "[9]");
        element(&mut b, 26, "TD", 22, "[10]");
        b.build(1)
    }

    /// Under structure-tree order the tags decide the blocks: the heading
    /// needs no size step, the paragraphs no gap, the list no marker
    /// pattern and the table no third row.
    // Covers ISO 32000-1 §14.8.3 and §14.8.4.3.
    #[test]
    fn tagged_block_elements_decide_the_markdown_under_structure_tree_order() {
        let doc = Document::load(tagged_blocks_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(
            md,
            "# Title\n\nFirst\n\nSecond\n\n1. One\n2. Two\n\n| a | b |\n| --- | --- |\n| c | d |"
        );
        let text = extract_text(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(text, "Title\nFirst\nSecond\n1. One\n2. Two\na b\nc d");
    }

    /// A tagged list whose items carry no Lbl element: the bullet glyph sits
    /// inside the LBody, on the item's line for the first item and on a
    /// line of its own for the second.
    fn tagged_unlabelled_list_doc() -> Vec<u8> {
        fn element(b: &mut PdfBuilder, num: u32, s: &str, parent: u32, kids: &str) {
            b.object(
                num,
                &format!("<< /Type /StructElem /S /{s} /P {parent} 0 R /Pg 3 0 R /K {kids} >>"),
            );
        }
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf \
              /LBody << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (\\225) Tj 1 0 0 1 90 700 Tm (One) Tj EMC \
              /LBody << /MCID 1 >> BDC 1 0 0 1 72 680 Tm (\\225) Tj EMC \
              /LBody << /MCID 2 >> BDC 1 0 0 1 90 664 Tm (Two) Tj EMC ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            10,
            "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
        );
        b.object(
            11,
            "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R] >>",
        );
        b.object(12, "<< /Nums [0 [16 0 R 17 0 R 17 0 R]] >>");
        element(&mut b, 13, "L", 11, "[14 0 R 15 0 R]");
        element(&mut b, 14, "LI", 13, "[16 0 R]");
        element(&mut b, 16, "LBody", 14, "[0]");
        element(&mut b, 15, "LI", 13, "[17 0 R]");
        element(&mut b, 17, "LBody", 15, "[1 2]");
        b.build(1)
    }

    /// Without a Lbl the marker is read off the item's first line as on an
    /// untagged page, and a line that is nothing but the marker is dropped
    /// so the item opens with its text.
    // Covers ISO 32000-1 §14.8.4.3.
    #[test]
    fn a_tagged_list_without_labels_reads_its_markers_off_the_lines() {
        let doc = Document::load(tagged_unlabelled_list_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(md, "- One\n- Two");
    }

    /// Three tagged lists with a `ListNumbering` attribute: an UpperAlpha
    /// list whose Lbl elements read A. and B., a Decimal list whose items
    /// have no Lbl and no marker in their text, and a LowerRoman list whose
    /// Lbl elements read ii. and iv.
    fn tagged_numbered_lists_doc() -> Vec<u8> {
        fn element(b: &mut PdfBuilder, num: u32, s: &str, parent: u32, kids: &str, extra: &str) {
            b.object(
                num,
                &format!(
                    "<< /Type /StructElem /S /{s} /P {parent} 0 R /Pg 3 0 R /K {kids} {extra} >>"
                ),
            );
        }
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf \
              /Lbl << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (A.) Tj EMC \
              /LBody << /MCID 1 >> BDC 1 0 0 1 90 700 Tm (First) Tj EMC \
              /Lbl << /MCID 2 >> BDC 1 0 0 1 72 686 Tm (B.) Tj EMC \
              /LBody << /MCID 3 >> BDC 1 0 0 1 90 686 Tm (Second) Tj EMC \
              /LBody << /MCID 4 >> BDC 1 0 0 1 72 650 Tm (Three) Tj EMC \
              /LBody << /MCID 5 >> BDC 1 0 0 1 72 636 Tm (Four) Tj EMC \
              /Lbl << /MCID 6 >> BDC 1 0 0 1 72 600 Tm (ii.) Tj EMC \
              /LBody << /MCID 7 >> BDC 1 0 0 1 90 600 Tm (Five) Tj EMC \
              /Lbl << /MCID 8 >> BDC 1 0 0 1 72 586 Tm (iv.) Tj EMC \
              /LBody << /MCID 9 >> BDC 1 0 0 1 90 586 Tm (Six) Tj EMC ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            10,
            "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
        );
        b.object(
            11,
            "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 20 0 R 25 0 R] >>",
        );
        b.object(
            12,
            "<< /Nums [0 [15 0 R 16 0 R 18 0 R 19 0 R 22 0 R 24 0 R 27 0 R 28 0 R 30 0 R 31 0 R]] >>",
        );
        element(
            &mut b,
            13,
            "L",
            11,
            "[14 0 R 17 0 R]",
            "/A << /O /List /ListNumbering /UpperAlpha >>",
        );
        element(&mut b, 14, "LI", 13, "[15 0 R 16 0 R]", "");
        element(&mut b, 15, "Lbl", 14, "[0]", "");
        element(&mut b, 16, "LBody", 14, "[1]", "");
        element(&mut b, 17, "LI", 13, "[18 0 R 19 0 R]", "");
        element(&mut b, 18, "Lbl", 17, "[2]", "");
        element(&mut b, 19, "LBody", 17, "[3]", "");
        element(
            &mut b,
            20,
            "L",
            11,
            "[21 0 R 23 0 R]",
            "/A << /O /List /ListNumbering /Decimal >>",
        );
        element(&mut b, 21, "LI", 20, "[22 0 R]", "");
        element(&mut b, 22, "LBody", 21, "[4]", "");
        element(&mut b, 23, "LI", 20, "[24 0 R]", "");
        element(&mut b, 24, "LBody", 23, "[5]", "");
        element(
            &mut b,
            25,
            "L",
            11,
            "[26 0 R 29 0 R]",
            "/A << /O /List /ListNumbering /LowerRoman >>",
        );
        element(&mut b, 26, "LI", 25, "[27 0 R 28 0 R]", "");
        element(&mut b, 27, "Lbl", 26, "[6]", "");
        element(&mut b, 28, "LBody", 26, "[7]", "");
        element(&mut b, 29, "LI", 25, "[30 0 R 31 0 R]", "");
        element(&mut b, 30, "Lbl", 29, "[8]", "");
        element(&mut b, 31, "LBody", 29, "[9]", "");
        b.build(1)
    }

    /// A list whose `ListNumbering` names a numbering system is numbered:
    /// an item by the number its label writes in that system, an unlabelled
    /// item by the number after the previous item's, so alphabetic and Roman
    /// labels and unlabelled items come out numbered.
    // Covers ISO 32000-1 §14.8.5.5.
    #[test]
    fn list_numbering_numbers_the_items_of_a_numbered_list() {
        let doc = Document::load(tagged_numbered_lists_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(
            md,
            "1. First\n2. Second\n\n1. Three\n2. Four\n\n2. Five\n4. Six"
        );
    }

    /// A Sect carrying `ListNumbering Decimal` for the two unlabelled lists
    /// inside it; the second list says `None` for itself.
    fn tagged_inherited_numbering_doc() -> Vec<u8> {
        fn element(b: &mut PdfBuilder, num: u32, s: &str, parent: u32, kids: &str, extra: &str) {
            b.object(
                num,
                &format!(
                    "<< /Type /StructElem /S /{s} /P {parent} 0 R /Pg 3 0 R /K {kids} {extra} >>"
                ),
            );
        }
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf \
              /LBody << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (One) Tj EMC \
              /LBody << /MCID 1 >> BDC 1 0 0 1 72 686 Tm (Two) Tj EMC \
              /LBody << /MCID 2 >> BDC 1 0 0 1 72 650 Tm (Three) Tj EMC \
              /LBody << /MCID 3 >> BDC 1 0 0 1 72 636 Tm (Four) Tj EMC ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            10,
            "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
        );
        b.object(
            11,
            "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R] >>",
        );
        b.object(12, "<< /Nums [0 [16 0 R 18 0 R 22 0 R 24 0 R]] >>");
        element(
            &mut b,
            13,
            "Sect",
            11,
            "[14 0 R 20 0 R]",
            "/A << /O /List /ListNumbering /Decimal >>",
        );
        element(&mut b, 14, "L", 13, "[15 0 R 17 0 R]", "");
        element(&mut b, 15, "LI", 14, "[16 0 R]", "");
        element(&mut b, 16, "LBody", 15, "[0]", "");
        element(&mut b, 17, "LI", 14, "[18 0 R]", "");
        element(&mut b, 18, "LBody", 17, "[1]", "");
        element(
            &mut b,
            20,
            "L",
            13,
            "[21 0 R 23 0 R]",
            "/A << /O /List /ListNumbering /None >>",
        );
        element(&mut b, 21, "LI", 20, "[22 0 R]", "");
        element(&mut b, 22, "LBody", 21, "[2]", "");
        element(&mut b, 23, "LI", 20, "[24 0 R]", "");
        element(&mut b, 24, "LBody", 23, "[3]", "");
        b.build(1)
    }

    /// `ListNumbering` is inheritable: a list without one takes the nearest
    /// ancestor's, and a list that says `None` for itself keeps its markers
    /// as they are.
    // Covers ISO 32000-1 §14.8.5.3 and §14.8.5.5.
    #[test]
    fn list_numbering_is_inherited_from_an_ancestor_unless_the_list_says_none() {
        let doc = Document::load(tagged_inherited_numbering_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(md, "1. One\n2. Two\n\n- Three\n- Four");
    }

    /// One list item whose Lbl sits on a line of its own and whose LBody
    /// holds a nested list with a dash label.
    fn tagged_nested_list_doc() -> Vec<u8> {
        fn element(b: &mut PdfBuilder, num: u32, s: &str, parent: u32, kids: &str) {
            b.object(
                num,
                &format!("<< /Type /StructElem /S /{s} /P {parent} 0 R /Pg 3 0 R /K {kids} >>"),
            );
        }
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf \
              /Lbl << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (\\225) Tj EMC \
              /LBody << /MCID 1 >> BDC 1 0 0 1 90 684 Tm (Outer) Tj EMC \
              /Lbl << /MCID 2 >> BDC 1 0 0 1 90 668 Tm (\\226) Tj EMC \
              /LBody << /MCID 3 >> BDC 1 0 0 1 108 668 Tm (Inner) Tj EMC ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            10,
            "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
        );
        b.object(
            11,
            "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R] >>",
        );
        b.object(12, "<< /Nums [0 [15 0 R 16 0 R 19 0 R 20 0 R]] >>");
        element(&mut b, 13, "L", 11, "[14 0 R]");
        element(&mut b, 14, "LI", 13, "[15 0 R 16 0 R]");
        element(&mut b, 15, "Lbl", 14, "[0]");
        element(&mut b, 16, "LBody", 14, "[1 17 0 R]");
        element(&mut b, 17, "L", 16, "[18 0 R]");
        element(&mut b, 18, "LI", 17, "[19 0 R 20 0 R]");
        element(&mut b, 19, "Lbl", 18, "[2]");
        element(&mut b, 20, "LBody", 18, "[3]");
        b.build(1)
    }

    /// An item's marker is its own Lbl, not the labels of the list nested
    /// inside it, whose content stays in the item as lines.
    // Covers ISO 32000-1 §14.8.4.3.
    #[test]
    fn a_nested_list_keeps_its_labels_out_of_the_outer_marker() {
        let doc = Document::load(tagged_nested_list_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(md, "- Outer\n\u{2013} Inner");
    }

    /// A tagged table whose cells declare `ColSpan` and `RowSpan` through
    /// their attribute objects.
    fn tagged_spanning_table_doc() -> Vec<u8> {
        fn element(b: &mut PdfBuilder, num: u32, s: &str, parent: u32, kids: &str, extra: &str) {
            b.object(
                num,
                &format!(
                    "<< /Type /StructElem /S /{s} /P {parent} 0 R /Pg 3 0 R /K {kids} {extra} >>"
                ),
            );
        }
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf \
              /TD << /MCID 0 >> BDC 1 0 0 1 72 600 Tm (a) Tj EMC \
              /TD << /MCID 1 >> BDC 1 0 0 1 72 586 Tm (c) Tj EMC \
              /TD << /MCID 2 >> BDC 1 0 0 1 200 586 Tm (d) Tj EMC \
              /TD << /MCID 3 >> BDC 1 0 0 1 200 572 Tm (e) Tj EMC ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            10,
            "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
        );
        b.object(
            11,
            "<< /Type /StructElem /S /Document /P 10 0 R /K [20 0 R] >>",
        );
        b.object(12, "<< /Nums [0 [23 0 R 25 0 R 26 0 R 28 0 R]] >>");
        element(&mut b, 20, "Table", 11, "[21 0 R 22 0 R 27 0 R]", "");
        element(&mut b, 21, "TR", 20, "[23 0 R]", "");
        element(&mut b, 23, "TD", 21, "[0]", "/A << /O /Table /ColSpan 2 >>");
        element(&mut b, 22, "TR", 20, "[25 0 R 26 0 R]", "");
        element(&mut b, 25, "TD", 22, "[1]", "/A << /O /Table /RowSpan 2 >>");
        element(&mut b, 26, "TD", 22, "[2]", "");
        element(&mut b, 27, "TR", 20, "[28 0 R]", "");
        element(&mut b, 28, "TD", 27, "[3]", "");
        b.build(1)
    }

    /// The cells' `ColSpan` and `RowSpan` reach the table, which renders as
    /// HTML because pipe tables cannot say either.
    // Covers ISO 32000-1 §14.8.5.7.
    #[test]
    fn tagged_cells_carry_their_column_and_row_spans() {
        let doc = Document::load(tagged_spanning_table_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(
            md,
            "<table>\n<tr><td colspan=\"2\">a</td></tr>\n\
             <tr><td rowspan=\"2\">c</td><td>d</td></tr>\n<tr><td>e</td></tr>\n</table>"
        );
        let text = extract_text(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(text, "a\nc d\ne");
    }

    /// A paragraph whose middle sequence sits in a Code element, the other
    /// two directly in the P.
    fn tagged_code_doc() -> Vec<u8> {
        fn element(b: &mut PdfBuilder, num: u32, s: &str, parent: u32, kids: &str) {
            b.object(
                num,
                &format!("<< /Type /StructElem /S /{s} /P {parent} 0 R /Pg 3 0 R /K {kids} >>"),
            );
        }
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf \
              /P << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (Run) Tj EMC \
              /Code << /MCID 1 >> BDC 1 0 0 1 100 700 Tm (ls -la) Tj EMC \
              /P << /MCID 2 >> BDC 1 0 0 1 150 700 Tm (now) Tj EMC ET",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            10,
            "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
        );
        b.object(
            11,
            "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R] >>",
        );
        b.object(12, "<< /Nums [0 [13 0 R 14 0 R 13 0 R]] >>");
        element(&mut b, 13, "P", 11, "[0 14 0 R 2]");
        element(&mut b, 14, "Code", 13, "[1]");
        b.build(1)
    }

    /// Text inside a Code element renders as inline code; plain text keeps
    /// the line as shown.
    // Covers ISO 32000-1 §14.8.4.4.
    #[test]
    fn code_elements_render_as_inline_code() {
        let doc = Document::load(tagged_code_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(md, "Run `ls -la` now");
        let text = extract_text(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(text, "Run ls -la now");
    }

    /// One tagged page built from `content` and a structure tree given as
    /// (object number, type, parent, kids) rows under the Document element
    /// 11, with the parent tree `nums`.
    fn tagged_tree_doc(content: &[u8], nums: &str, elements: &[(u32, &str, u32, &str)]) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", content);
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            10,
            "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
        );
        let top: Vec<String> = elements
            .iter()
            .filter(|(_, _, parent, _)| *parent == 11)
            .map(|(num, _, _, _)| format!("{num} 0 R"))
            .collect();
        b.object(
            11,
            &format!(
                "<< /Type /StructElem /S /Document /P 10 0 R /K [{}] >>",
                top.join(" ")
            ),
        );
        b.object(12, &format!("<< /Nums [0 [{nums}]] >>"));
        for (num, s, parent, kids) in elements {
            b.object(
                *num,
                &format!("<< /Type /StructElem /S /{s} /P {parent} 0 R /Pg 3 0 R /K {kids} >>"),
            );
        }
        b.build(1)
    }

    /// The paragraphs of a BlockQuote element render as a Markdown block
    /// quote; plain text keeps the lines as shown.
    // Covers ISO 32000-1 §14.8.4.2.
    #[test]
    fn block_quote_paragraphs_render_as_markdown_quotes() {
        let doc = Document::load(tagged_tree_doc(
            b"BT /F1 12 Tf \
              /P << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (A quoted line) Tj EMC \
              /P << /MCID 1 >> BDC 1 0 0 1 72 686 Tm (and its second) Tj EMC \
              /P << /MCID 2 >> BDC 1 0 0 1 72 660 Tm (Body) Tj EMC ET",
            "21 0 R 21 0 R 13 0 R",
            &[
                (20, "BlockQuote", 11, "[21 0 R]"),
                (21, "P", 20, "[0 1]"),
                (13, "P", 11, "[2]"),
            ],
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(md, "> A quoted line\n> and its second\n\nBody");
        let text = extract_text(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(text, "A quoted line\nand its second\nBody");
    }

    /// TOCI entries a line apart, with no block-level element inside, are
    /// blocks of their own instead of one paragraph, and a NonStruct wrapper
    /// changes nothing.
    // Covers ISO 32000-1 §14.8.4.2.
    #[test]
    fn toc_entries_are_their_own_blocks() {
        let doc = Document::load(tagged_tree_doc(
            b"BT /F1 12 Tf \
              /TOCI << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (Chapter 1 ... 3) Tj EMC \
              /TOCI << /MCID 1 >> BDC 1 0 0 1 72 686 Tm (Chapter 2 ... 9) Tj EMC \
              /P << /MCID 2 >> BDC 1 0 0 1 72 660 Tm (Body) Tj EMC ET",
            "21 0 R 22 0 R 31 0 R",
            &[
                (20, "TOC", 11, "[21 0 R 22 0 R]"),
                (21, "TOCI", 20, "[0]"),
                (22, "TOCI", 20, "[1]"),
                (30, "NonStruct", 11, "[31 0 R]"),
                (31, "P", 30, "[2]"),
            ],
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(md, "Chapter 1 ... 3\n\nChapter 2 ... 9\n\nBody");
    }

    /// Illustration text stays apart from untagged text: an untagged line
    /// followed by two figures' labels and a formula makes two blocks, the
    /// illustrations one run laid out by the heuristics, and the first
    /// figure's description reaches its spans.
    // Covers ISO 32000-1 §14.8.4.5.
    #[test]
    fn illustration_elements_are_their_own_blocks() {
        let doc = Document::load(tagged_tree_doc(
            b"BT /F1 12 Tf 1 0 0 1 72 714 Tm (Draft) Tj \
              /Figure << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (Chart A) Tj EMC \
              /Figure << /MCID 1 >> BDC 1 0 0 1 72 686 Tm (Chart B) Tj EMC \
              /Formula << /MCID 2 >> BDC 1 0 0 1 72 672 Tm (E = mc2) Tj EMC ET",
            "20 0 R 21 0 R 22 0 R",
            &[
                (20, "Figure", 11, "[0] /Alt (Sales by quarter)"),
                (21, "Figure", 11, "[1]"),
                (22, "Formula", 11, "[2]"),
            ],
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::StructureTree).unwrap();
        assert_eq!(md, "Draft\n\nChart A\nChart B\nE = mc2");
        let (spans, _) =
            pdfboss_text::extract_spans_reporting(&doc, &page, ReadingOrder::StructureTree)
                .unwrap();
        assert_eq!(spans[0].alt, None);
        assert_eq!(spans[1].alt.as_deref(), Some("Sales by quarter"));
        assert_eq!(spans[2].alt, None);
    }

    /// The same page in content order goes through the layout heuristics,
    /// which see no heading, one paragraph and no table.
    // Covers ISO 32000-1 §14.8.4.3.
    #[test]
    fn the_same_page_in_content_order_reads_by_the_layout_heuristics() {
        let doc = Document::load(tagged_blocks_doc()).unwrap();
        let page = doc.page(0).unwrap();
        let md = extract_page_markdown(&doc, &page, ReadingOrder::Content).unwrap();
        assert!(!md.contains('#'), "{md}");
        assert!(!md.contains('|'), "{md}");
        assert!(md.contains("First\nSecond"), "{md}");
        assert!(md.contains("1. One\n2. Two"), "{md}");
    }

    /// An untagged document asked for structure-tree order reads exactly as
    /// it does in content order, and its report says so.
    #[test]
    fn structure_tree_order_on_an_untagged_document_is_content_order() {
        let doc = Document::load(multi_page_doc(&["alpha", "beta"])).unwrap();
        for index in 0..2 {
            let page = doc.page(index).unwrap();
            let (tree, report) =
                extract_text_reporting(&doc, &page, ReadingOrder::StructureTree).unwrap();
            let (content, _) = extract_text_reporting(&doc, &page, ReadingOrder::Content).unwrap();
            assert_eq!(tree, content);
            assert_eq!(report.order, ReadingOrder::Content);
        }
    }

    /// A document's pages each keep their own order: a tagged page reads by
    /// its tree while an untagged neighbour reads its stream, in one layout.
    #[test]
    fn document_layout_orders_each_page_by_its_own_order() {
        let tagged = Document::load(tagged_two_column_doc()).unwrap();
        let tagged_page = tagged.page(0).unwrap();
        let (tree_spans, report) = pdfboss_text::extract_spans_reporting(
            &tagged,
            &tagged_page,
            ReadingOrder::StructureTree,
        )
        .unwrap();
        let plain = Document::load(doc_with_graphics(
            "BT /F1 12 Tf 1 0 0 1 72 680 Tm (second) Tj 1 0 0 1 72 700 Tm (first) Tj ET",
        ))
        .unwrap();
        let plain_page = plain.page(0).unwrap();
        let (plain_spans, _) =
            pdfboss_text::extract_spans_reporting(&plain, &plain_page, ReadingOrder::Content)
                .unwrap();
        let layouts = document_layout(&[
            (tree_spans, report.order),
            (plain_spans, ReadingOrder::Content),
        ]);
        assert_eq!(
            Text.render(&layouts),
            "L1\nL2\nL3\nL4\nR1\nR2\nR3\nR4\u{c}first\nsecond"
        );
    }

    /// Non-whitespace token runs, counted — the content-preservation
    /// currency of the ruled oracle branch.
    fn token_counts(text: &str) -> std::collections::BTreeMap<&str, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for token in text.split_whitespace() {
            *counts.entry(token).or_default() += 1;
        }
        counts
    }

    /// The Text adapter over a ruling-free layout must reproduce the pre-IR
    /// string builder exactly — the local form of the corpus parity gate.
    /// [`structure::layout_reference`] is that builder, kept as the oracle.
    ///
    /// A ruling-fed layout genuinely reorders text — a merged logical row
    /// reads cell-major where the flat flow reads line-major — so the ruled
    /// branch asserts content preservation instead: the multiset of
    /// non-whitespace token runs equals the flat flow's, counted exactly.
    /// Loss, duplication, and fused tokens all break the count.
    #[test]
    fn text_adapter_matches_layout_on_fixtures() {
        let mut headings = 0usize;
        let mut splits = 0usize;
        let mut tables = 0usize;
        let mut ruled_tables = 0usize;
        for content in structure::tests::fixture_contents() {
            let doc = Document::load(doc_with_graphics(&content)).unwrap();
            let page = doc.page(0).unwrap();
            let (spans, rulings, report) = pdfboss_text::extract_spans_and_rulings_reporting(
                &doc,
                &page,
                ReadingOrder::Content,
            )
            .unwrap();
            assert!(report.is_complete(), "unexpected skips: {report:?}");
            let layout = page_layout_with_rulings(&spans, &rulings, ReadingOrder::Content);
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
            let fixture_tables = layout
                .blocks
                .iter()
                .filter(|block| matches!(block, Block::Table { .. }))
                .count();
            tables += fixture_tables;
            if !rulings.is_empty() {
                ruled_tables += fixture_tables;
            }
            let via_ir = Text.render(&[layout]);
            let flat = structure::layout_reference(&spans, ReadingOrder::Content);
            if rulings.is_empty() {
                assert_eq!(via_ir, flat, "content: {content}");
            } else {
                assert_eq!(
                    token_counts(&via_ir),
                    token_counts(&flat),
                    "content: {content}\nvia IR: {via_ir}\nflat flow: {flat}"
                );
            }
        }
        // A fixture set that classifies nothing would pass this test without
        // ever reaching the code it guards.
        assert!(headings > 0, "no fixture produced a heading block");
        assert!(splits > 0, "no fixture split into several paragraphs");
        assert!(tables > 0, "no fixture produced a table block");
        assert!(ruled_tables > 0, "no fixture produced a ruled table block");
    }

    /// Markdown of a one-page document with `content` as its raw content
    /// stream, through document-level size statistics.
    fn markdown_of(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, report) =
            pdfboss_text::extract_spans_reporting(&doc, &page, ReadingOrder::Content).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        Markdown.render(&document_layout(&[(spans, ReadingOrder::Content)]))
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
        let (spans, report) =
            pdfboss_text::extract_spans_reporting(&doc, &page, ReadingOrder::Content).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        Markdown.render(&document_layout(&[(spans, ReadingOrder::Content)]))
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

    #[test]
    fn bullet_lines_become_list_items() {
        let content = "BT /F1 12 Tf 72 720 Td (\\225 first item) Tj \
                       0 -14 Td (\\225 second item) Tj \
                       0 -14 Td (Body sentence after the list ends here.) Tj ET";
        // \225 is bullet in WinAnsi. Fixture font must be WinAnsi-encoded.
        let md = markdown_of(content);
        assert!(md.contains("- first item\n- second item"), "md: {md}");
        assert!(!md.contains('\u{2022}'), "marker replaced, not kept: {md}");
    }

    /// A marker line whose candidate falls short of the list minimum stays
    /// prose, and the scan resumes at the very next line — a later list on
    /// the same run must still form.
    #[test]
    fn a_lone_marker_line_stays_prose() {
        let content = "BT /F1 12 Tf 72 720 Td (- stray dash line) Tj \
                       0 -14 Td (Body sentence at the same indent.) Tj \
                       0 -14 Td (- alpha) Tj 0 -14 Td (- beta) Tj ET";
        let md = markdown_of(content);
        assert!(md.contains("- alpha\n- beta"), "md: {md}");
        assert!(
            md.contains("- stray dash line\nBody sentence at the same indent."),
            "the stray marker line stays in the paragraph: {md}"
        );
    }

    #[test]
    fn numbered_items_keep_their_numbers() {
        let content = "BT /F1 12 Tf 72 720 Td (1. alpha) Tj 0 -14 Td (2. beta) Tj \
                       0 -14 Td (12) Tj ET";
        let md = markdown_of(content);
        assert!(md.contains("1. alpha\n2. beta"), "md: {md}");
        assert!(
            md.contains("12"),
            "a bare number line is not a list item: {md}"
        );
    }

    #[test]
    fn hanging_indent_continues_an_item() {
        // Continuation line starts right of the marker column.
        let content = "BT /F1 12 Tf 72 720 Td (\\225 a long item that) Tj \
                       10 -14 Td (wraps to a second line) Tj ET";
        let md = markdown_of(content);
        assert!(
            md.contains("- a long item that\nwraps to a second line")
                || md.contains("- a long item that wraps to a second line"),
            "md: {md}"
        );
    }

    /// Three lanes, four aligned rows -> one pipe table.
    #[test]
    fn lane_grid_becomes_pipe_table() {
        let md = markdown_of(&structure::tests::lane_grid_content());
        assert!(md.contains("| r0c0 | r0c1 | r0c2 |"), "md: {md}");
        assert!(
            md.contains("| --- | --- | --- |"),
            "separator after header: {md}"
        );
        assert!(md.contains("| r3c0 | r3c1 | r3c2 |"), "md: {md}");
    }

    /// An eight-point column gap on a page-wide stretch is real table
    /// structure: exact interval lanes must keep resolving it where a
    /// binned occupancy histogram rounded it away.
    #[test]
    fn a_narrow_column_gap_still_opens_a_lane() {
        let md = markdown_of(&structure::tests::narrow_gap_lane_grid_content());
        assert!(md.contains("| r0c0 | r0c1 | r0c2 |"), "md: {md}");
        assert!(md.contains("| r3c0 | r3c1 | r3c2 |"), "md: {md}");
    }

    /// Page-edge lines sharing the band with a grid leave as prose: the
    /// running header does not take the header row's place — which, being
    /// wide enough to cross every lane, would also flip the block to the HTML
    /// dialect as a merged cell — and the page number is not a last row. A
    /// single-cell line between two rows is a wrapped cell and stays a row.
    #[test]
    fn page_edge_lines_around_the_grid_stay_prose() {
        let md = markdown_of(&structure::tests::grid_with_edge_lines_content());
        let header = structure::tests::RUNNING_HEADER;
        assert!(
            !md.contains("<table>"),
            "an edge line flipped the dialect: {md}"
        );
        assert!(
            md.contains(&format!(
                "{header}\n\n| r0c0 | r0c1 | r0c2 |\n| --- | --- | --- |"
            )),
            "md: {md}"
        );
        assert!(
            md.contains("| r1c0 | r1c1 | r1c2 |\n| wrapped cell |  |  |\n| r2c0 |"),
            "wrapped cell is a row: {md}"
        );
        assert!(md.contains("| r3c0 | r3c1 | r3c2 |"), "md: {md}");
        assert!(!md.contains("| 24 |"), "page number is not a row: {md}");
        assert!(md.ends_with("\n\n24"), "md: {md}");
    }

    /// A page footer populating two of the grid's columns far below it is
    /// not a row: the grid ends at the last evenly pitched row and the
    /// footer reads as prose. Modeled on a 10-K statement page whose
    /// "Form 10-K  41" footer used to void the whole statement.
    #[test]
    fn a_far_footer_row_does_not_void_the_grid() {
        let md = markdown_of(&structure::tests::grid_with_far_footer_content());
        assert!(md.contains("| r0c0 | r0c1 | r0c2 |"), "md: {md}");
        assert!(md.contains("| r3c0 | r3c1 | r3c2 |"), "md: {md}");
        assert!(!md.contains("| Form 10-K |"), "footer is not a row: {md}");
        assert!(md.ends_with("\n\nForm 10-K 41"), "md: {md}");
    }

    /// A title set in a heading size over two side-by-side grids is not
    /// the table's first row: it leaves as the heading it is, and the grid
    /// starts at its header. Modeled on a rate manual whose "Symbols" title
    /// became a header row and lost every column match.
    #[test]
    fn a_title_over_side_by_side_grids_is_a_heading_not_a_row() {
        let md = markdown_of(&structure::tests::titled_side_by_side_grids_content());
        assert!(md.contains("# Symbols Symbols"), "md: {md}");
        assert!(
            md.contains("| r0c0 | r0c1 | r0c2 | r0c3 | r0c4 | r0c5 |"),
            "md: {md}"
        );
        assert!(!md.contains("| Symbols |"), "title is not a row: {md}");
    }

    /// A "$" set in its own column, left-aligned ahead of a right-aligned
    /// amount, belongs to that amount: the cells read "$1,824", and the
    /// column the signs stood in is gone.
    #[test]
    fn currency_signs_rejoin_their_amounts() {
        let md = markdown_of(&structure::tests::currency_columns_content());
        assert!(
            md.contains("| Gross | $1,824 | $1,889 | $1,978 |"),
            "md: {md}"
        );
        assert!(
            md.contains("| Net | $1,702 | $1,777 | $1,840 |"),
            "md: {md}"
        );
        assert!(md.contains("| Paid | $122 | $112 | $138 |"), "md: {md}");
        assert!(
            md.contains("| --- | --- | --- | --- |"),
            "four columns: {md}"
        );
        assert!(!md.contains("| $ |"), "no sign column: {md}");
    }

    /// A euro sign after its amount stays there: "7 723 €" is how most of
    /// Europe writes it, and the sign is not the next cell's. Modeled on a
    /// ParseBench page where "€" jumped onto the rate beside it.
    #[test]
    fn a_trailing_euro_stays_with_its_amount() {
        let md = markdown_of(&structure::tests::euro_suffix_content());
        assert!(md.contains("| Nord | 7 723 € | 51,4% |"), "md: {md}");
        assert!(md.contains("| Est | 6 734 € | 50,7% |"), "md: {md}");
    }

    /// A ")" or "%" set apart from its amount, in the next cell or a word
    /// gap away, closes the amount: "(1,234)", "(5)" and "12%".
    #[test]
    fn closers_rejoin_their_amounts() {
        let md = markdown_of(&structure::tests::split_closers_content());
        assert!(md.contains("| Loss | (1,234) | (5) |"), "md: {md}");
        assert!(md.contains("| Gain | 1,889 | 7 |"), "md: {md}");
        assert!(md.contains("| Rate | 12% | 3 |"), "md: {md}");
        assert!(md.contains("| --- | --- | --- |"), "three columns: {md}");
    }

    /// A whitespace span standing in a gutter is padding, not a column:
    /// the grid keeps its three columns and no empty one appears between
    /// them. Modeled on filing and 10-K pages whose padding spans opened an
    /// empty column beside every value.
    #[test]
    fn padding_in_a_gutter_opens_no_column() {
        let md = markdown_of(&structure::tests::padded_gutter_grid_content());
        assert!(md.contains("| r0c0 | r0c1 | r0c2 |"), "md: {md}");
        assert!(md.contains("| --- | --- | --- |\n"), "three columns: {md}");
        assert!(!md.contains("|  |"), "no empty column: {md}");
    }

    /// Entry numbers, titles and climbing page numbers in three lanes are a
    /// table of contents, which reads as prose lines, not as a grid. Two
    /// entries sharing a page keep it a list. Modeled on an odl page whose
    /// contents list became a table.
    #[test]
    fn a_contents_list_is_not_a_table() {
        let md = markdown_of(&structure::tests::contents_list_content());
        assert!(!md.contains("| --- |"), "no table: {md}");
        assert!(md.contains("1. Front Matter 1"), "md: {md}");
        assert!(md.contains("3. Our Mental Shortcuts 3"), "md: {md}");
        assert!(md.contains("4. Identifying a Topic 25"), "md: {md}");
    }

    /// A section label standing a blank line above its rows, populating the
    /// first cell alone, is a row of the table around it, not the end of
    /// one table and the start of another.
    #[test]
    fn a_section_label_stays_inside_the_lane_table() {
        let md = markdown_of(&structure::tests::sectioned_lane_table_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            1,
            "one table: {md}"
        );
        assert!(
            md.contains(
                "| r2c0 | r2c1 | r2c2 |\n| Paid-in Capital: |  |  |\n| r3c0 | r3c1 | r3c2 |"
            ),
            "md: {md}"
        );
    }

    /// Column gaps of five points in 7-point type are lanes: the gutter
    /// minimum scales down with small type.
    #[test]
    fn narrow_gaps_in_small_type_are_lanes() {
        let md = markdown_of(&structure::tests::tight_lane_grid_content());
        assert!(
            md.contains("| r0c0 | r0c1 | r0c2 |\n| --- | --- | --- |\n| r1c0 | r1c1 | r1c2 |"),
            "md: {md}"
        );
    }

    /// A short grid above the run's longest stretch is a table too: the
    /// lines above the band get the same attempt as the lines below it.
    #[test]
    fn a_short_grid_above_a_longer_one_is_a_table_too() {
        let md = markdown_of(&structure::tests::grid_above_a_longer_grid_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            2,
            "two tables: {md}"
        );
        assert!(md.contains("| r1c0 | r1c1 | r1c2 |"), "md: {md}");
        assert!(md.contains("| s4c0 | s4c1 | s4c2 |"), "md: {md}");
        assert!(md.contains("Prose between the two grids."), "md: {md}");
    }

    /// Two lanes of short terms and definitions are a table: a glossary,
    /// a subsidiary list, a rate factor table.
    #[test]
    fn a_two_column_glossary_is_a_table() {
        let md = markdown_of(&structure::tests::glossary_content());
        assert!(
            md.contains(
                "| Term | Definition |\n| --- | --- |\n| AED | Advanced Electronic Data |\n\
                 | AFC | Audit and Finance Committee of the Board |"
            ),
            "md: {md}"
        );
        assert!(
            md.contains("| Board | Board of Governors of the Postal Service |"),
            "md: {md}"
        );
    }

    /// A sign opening a cell closes up to the amount after its padding in
    /// the Markdown: "$   1,414.00" reads "$1,414.00".
    #[test]
    fn a_padded_sign_closes_up_to_its_amount() {
        let md = markdown_of(&structure::tests::padded_sign_amount_lane_content());
        assert!(
            md.contains("| Cash | $1,414.00 | 10% |\n| Debt | $2,120.50 | 9% |"),
            "md: {md}"
        );
    }

    /// Markers a lane's width from their text are a list, not a two-column
    /// table.
    #[test]
    fn a_numbered_list_in_lanes_is_not_a_table() {
        let md = markdown_of(&structure::tests::numbered_lane_list_content());
        assert!(!md.contains("| ---"), "no table: {md}");
        assert!(md.contains("1. Item number 1 of the list"), "md: {md}");
        assert!(md.contains("5. Item number 5 of the list"), "md: {md}");
    }

    /// A two-column table set inside a numbered item's one-lane stretch is
    /// found on its own: the item's stretch settles only the starts that
    /// leave the item's lane.
    #[test]
    fn a_two_column_table_inside_a_numbered_item_is_found() {
        let md = markdown_of(&structure::tests::list_then_two_column_table_content());
        assert!(md.contains("| Loss Costs | GL-2013-BGL1 |"), "md: {md}");
        assert!(md.contains("| Loss Costs | CR-2007-RLA1 |"), "md: {md}");
        assert!(
            md.contains("1. Adopt the following reference filings:"),
            "md: {md}"
        );
    }

    /// A row repeating the head's years over a new first head opens a
    /// second table: two tables at one pitch are two, not one.
    #[test]
    fn a_row_repeating_the_head_opens_a_second_table() {
        let md = markdown_of(&structure::tests::two_tables_repeating_their_head_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            2,
            "two tables: {md}"
        );
        assert!(
            md.contains("| At year end | 2018 | 2017 |\n| --- | --- | --- |\n| Total assets |"),
            "md: {md}"
        );
    }

    /// A table of contents whose front matter counts pages in roman
    /// numerals and whose parts are headed by a line of words alone is
    /// prose, not a two-column table.
    #[test]
    fn a_contents_list_with_front_matter_is_not_a_table() {
        let md = markdown_of(&structure::tests::front_matter_contents_content());
        assert!(!md.contains("| ---"), "md: {md}");
        assert!(md.contains("About the Publisher vii"), "md: {md}");
        assert!(
            md.contains("Experiment 3: Energy Loss in Pipes 24"),
            "md: {md}"
        );
    }

    /// A table of contents ruled under each entry is prose under its rules
    /// as under its lanes.
    #[test]
    fn a_contents_list_under_rules_is_prose() {
        let md = markdown_of_drawn(&structure::tests::ruled_contents_content());
        assert!(!md.contains("| ---"), "md: {md}");
        assert!(!md.contains("<table>"), "md: {md}");
        assert!(md.contains("Executive Summary 4"), "md: {md}");
    }

    /// A last column ending in "ml" spells a roman numeral far past any
    /// front matter, and "llc" spells none in standard form: the volumes
    /// table stays a table.
    #[test]
    fn a_table_of_volumes_is_not_a_contents_list() {
        let md = markdown_of(&structure::tests::volumes_table_content());
        assert!(md.contains("| 2 | 24 ml | 0 ml | 4 ml |"), "md: {md}");
        assert!(md.contains("| 4 | 4 ml | 12 ml | 12 ml |"), "md: {md}");
    }

    /// Two columns of prose too short for the gutter pass share one lane;
    /// neither side is narrow, so they are not a two-column table.
    #[test]
    fn two_prose_columns_are_not_a_table() {
        let md = markdown_of(&structure::tests::two_prose_columns_content());
        assert!(!md.contains("| ---"), "no table: {md}");
        assert!(
            md.contains("The left column runs its text to and the right column does the same"),
            "md: {md}"
        );
    }

    /// References marked "(10)" to "(14)" a lane from their text are a
    /// bibliography, not a two-column table.
    #[test]
    fn a_bibliography_in_lanes_is_not_a_table() {
        let md = markdown_of(&structure::tests::bracketed_reference_list_content());
        assert!(!md.contains("| ---"), "no table: {md}");
        assert!(
            md.contains("(10) Handbook of Chemistry, edition 10"),
            "md: {md}"
        );
    }

    /// A second grid below the first, in the same segment, is a second
    /// table: the stretch below a table gets the same attempt.
    #[test]
    fn a_second_grid_below_the_first_is_a_second_table() {
        let md = markdown_of(&structure::tests::two_grids_content());
        assert!(md.contains("| r0c0 | r0c1 | r0c2 |"), "md: {md}");
        assert!(md.contains("| r2c0 | r2c1 | r2c2 |"), "md: {md}");
        assert!(md.contains("| s0c0 | s0c1 | s0c2 |"), "md: {md}");
        assert!(md.contains("| s2c0 | s2c1 | s2c2 |"), "md: {md}");
        assert_eq!(
            md.matches("| --- | --- | --- |").count(),
            2,
            "two tables: {md}"
        );
    }

    /// A lane held open by a page number out in the margin is not a cell
    /// column: hoisting the number empties it, and the four rows of short
    /// cells are a two-column table with no third cell. Modeled on a bench
    /// page whose two-column pitch read as a three-column table with an
    /// empty third cell in every row.
    #[test]
    fn a_margin_page_number_does_not_manufacture_a_column() {
        let md = markdown_of(&structure::tests::margin_number_grid_content());
        assert!(
            md.contains("| r0c0 | r0c1 |\n| --- | --- |\n| r1c0 | r1c1 |"),
            "two columns, no third: {md}"
        );
        assert!(!md.contains("| --- | --- | --- |"), "no third column: {md}");
        assert!(md.ends_with("\n\n3"), "the page number survives: {md}");
    }

    /// A cell crossing the lane gap forces the HTML dialect with colspan.
    #[test]
    fn spanning_cell_switches_to_html_table() {
        // Same 4x3 grid as lane_grid_becomes_pipe_table, except row 0's first
        // cell is one long string whose advance (testkit default width 500 →
        // 5pt/char at 10pt) runs from x=72 past lane 1's start at x=250.
        let mut content = String::from(
            "BT /F1 10 Tf 1 0 0 1 72 700 Tm (a merged header cell spanning two lanes xx) Tj \
             1 0 0 1 430 700 Tm (r0c2) Tj ",
        );
        for (row, y) in [(1, 680.0), (2, 660.0), (3, 640.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "ET";
        let md = markdown_of(&content);
        assert!(md.contains("<table>"), "md: {md}");
        assert!(md.contains("colspan=\"2\""), "md: {md}");
        assert!(!md.contains("| r1c0 |"), "one table, one dialect: {md}");
    }

    /// Markdown of a one-page document whose content stream draws rulings,
    /// through the real document entry point, which threads them.
    fn markdown_of_drawn(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        extract_markdown(&doc, ReadingOrder::Content).unwrap()
    }

    /// One drawn grid whose cell text arrives as two flows (the bottom rows
    /// written before the top ones) is still one table: flows sharing a
    /// grid merge before segmentation, so the grid cannot fragment into a
    /// table per flow.
    #[test]
    fn a_grid_written_in_two_flows_is_one_table() {
        let md = markdown_of_drawn(
            "70 630 360 80 re S 250 630 m 250 710 l S \
             70 690 m 430 690 l S 70 670 m 430 670 l S 70 650 m 430 650 l S \
             BT /F1 10 Tf 1 0 0 1 80 655 Tm (a3) Tj 1 0 0 1 260 655 Tm (b3) Tj \
             1 0 0 1 80 635 Tm (a4) Tj 1 0 0 1 260 635 Tm (b4) Tj \
             1 0 0 1 80 695 Tm (a1) Tj 1 0 0 1 260 695 Tm (b1) Tj \
             1 0 0 1 80 675 Tm (a2) Tj 1 0 0 1 260 675 Tm (b2) Tj ET",
        );
        assert_eq!(
            md.matches("| --- | --- |").count(),
            1,
            "one grid must be one table: {md:?}"
        );
        assert!(
            md.contains("| a1 | b1 |\n| --- | --- |\n| a2 | b2 |\n| a3 | b3 |\n| a4 | b4 |"),
            "rows read top to bottom in one table: {md:?}"
        );
    }

    /// A trailing whitespace-only span running past the grid's right edge —
    /// a producer's padding — paints nothing and must not disqualify the
    /// row, and with it the whole grid's claim.
    #[test]
    fn trailing_space_span_does_not_fail_a_grid_row() {
        let md = markdown_of_drawn(
            "70 670 360 40 re S 250 670 m 250 710 l S 70 690 m 430 690 l S \
             BT /F1 10 Tf 1 0 0 1 80 695 Tm (a1) Tj 1 0 0 1 260 695 Tm (b1) Tj \
             1 0 0 1 80 675 Tm (a2) Tj 1 0 0 1 260 675 Tm (b2) Tj \
             1 0 0 1 480 675 Tm (                    ) Tj ET",
        );
        assert!(
            md.contains("| a1 | b1 |\n| --- | --- |\n| a2 | b2 |"),
            "the padded row still rows: {md:?}"
        );
    }

    /// A table ruled only horizontally — top rule, one under the header,
    /// bottom rule, no verticals — with its columns readable from the text:
    /// the open-ruled species most tables in print actually are.
    #[test]
    fn a_horizontally_ruled_table_becomes_a_table() {
        let md = markdown_of_drawn(
            "70 710 m 430 710 l S 70 688 m 430 688 l S 70 610 m 430 610 l S \
             BT /F1 10 Tf 1 0 0 1 72 700 Tm (Added cation) Tj 1 0 0 1 260 700 Tm (Relative rates) Tj \
             1 0 0 1 72 676 Tm (K+) Tj 1 0 0 1 260 676 Tm (slow) Tj \
             1 0 0 1 72 656 Tm (Na+) Tj 1 0 0 1 260 656 Tm (medium) Tj \
             1 0 0 1 72 636 Tm (Ca2+) Tj 1 0 0 1 260 636 Tm (fast) Tj \
             1 0 0 1 72 616 Tm (Check) Tj 1 0 0 1 260 616 Tm (none) Tj ET",
        );
        assert!(
            md.contains("| Added cation | Relative rates |"),
            "the header row rows: {md:?}"
        );
        assert!(
            md.contains("| K+ | slow |") && md.contains("| Check | none |"),
            "body rows read as rows: {md:?}"
        );
    }

    /// An open-ruled statement rules its header and its first section's
    /// last row and leaves the sections below unruled: the claim grows over
    /// every line that keeps the lanes, the section label a row of it, and
    /// stops at the note and the prose under the table.
    #[test]
    fn an_open_ruled_claim_grows_over_the_unruled_sections() {
        let md = markdown_of_drawn(&structure::tests::open_ruled_sections_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            1,
            "one table: {md}"
        );
        assert!(
            md.contains(
                "| Skiffia | francesae |\n| Asset Class |  |\n| Goodeid | atripinnis |\n\
                 | Splitfin | multiradiatus |\n| Total | five |"
            ),
            "md: {md}"
        );
        assert!(
            md.contains("In the table above:") && !md.contains("| In the table"),
            "the note stays prose: {md}"
        );
    }

    /// Two three-column tables whose header rules and total rules pair up
    /// into open lattices spanning both stay two tables of three columns: a
    /// lattice grows over unruled lines only from lines its own rules
    /// already read as rows.
    #[test]
    fn two_tables_sharing_an_open_lattice_stay_two() {
        let md = markdown_of_drawn(&structure::tests::two_tables_sharing_an_open_lattice_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            2,
            "two tables: {md}"
        );
        assert_eq!(
            md.matches("| Concentrate | 59 | 58 |").count(),
            2,
            "md: {md}"
        );
    }

    /// A lane every row crosses with a currency sign alone parts no cells:
    /// the sign and its padded amount are one cell, and the Markdown reads
    /// them closed up.
    #[test]
    fn a_lane_crossed_by_signs_alone_splits_no_drawn_column() {
        let md = markdown_of_drawn(&structure::tests::sign_padded_amount_ruled_content());
        assert!(
            md.contains(
                "| Item | Amount |\n| --- | --- |\n| Cash | $1,414.00 |\n| Debt | $2,120.50 |"
            ),
            "md: {md}"
        );
    }

    /// An open lattice whose first row is a title over both columns keeps
    /// its claim, the title a spanning cell and every line below it a row.
    #[test]
    fn a_title_atop_an_open_lattice_keeps_the_claim() {
        let md = markdown_of_drawn(&structure::tests::titled_open_lattice_content());
        assert!(
            md.contains("<td colspan=\"2\">Fish species on IUCN Red List</td>"),
            "md: {md}"
        );
        assert!(
            md.contains("<tr><td>Potosi Pupfish</td><td>Cyprinodon alvarezi</td></tr>"),
            "md: {md}"
        );
        assert!(
            md.contains("<tr><td>Golden Skiffia</td><td>Skiffia francesae</td></tr>"),
            "md: {md}"
        );
    }

    /// A chart frame welded onto a table through a shared vertical is no
    /// table of the axis labels beside it: the lattice claims nothing and
    /// the table comes out of the lanes.
    #[test]
    fn a_chart_frame_welded_onto_a_table_claims_no_rows() {
        let md = markdown_of_drawn(&structure::tests::chart_over_table_content());
        assert!(!md.contains("<table>"), "md: {md}");
        assert!(
            md.contains("| Rate | 2024 | 2023 |\n| --- | --- | --- |\n| PIF | 0.1% | 6.9% |"),
            "md: {md}"
        );
    }

    /// Two tables ruled across the full width with a line of prose between
    /// them chain into one open lattice; the prose reads as one cell over
    /// every column, which makes the lattice two tables with prose between.
    #[test]
    fn prose_inside_an_open_lattice_splits_it_into_two_tables() {
        let md = markdown_of_drawn(&structure::tests::two_tables_in_one_open_lattice_content());
        assert!(!md.contains("<table>"), "no merged table: {md}");
        assert_eq!(
            md.matches("| Cash | 36,364 | 48,677 |").count(),
            2,
            "md: {md}"
        );
        assert!(
            md.contains("\nThe table below presents details about our loans.\n"),
            "md: {md}"
        );
    }

    /// Two stacked open-ruled tables share their x-extent; the cluster
    /// splits at the largest rule gap and each table comes out whole.
    #[test]
    fn stacked_open_ruled_tables_split_apart() {
        let md = markdown_of_drawn(
            "70 710 m 430 710 l S 70 688 m 430 688 l S 70 648 m 430 648 l S \
             70 470 m 430 470 l S 70 448 m 430 448 l S 70 408 m 430 408 l S \
             BT /F1 10 Tf 1 0 0 1 72 700 Tm (Name) Tj 1 0 0 1 260 700 Tm (Kind) Tj \
             1 0 0 1 72 676 Tm (Pupfish) Tj 1 0 0 1 260 676 Tm (alvarezi) Tj \
             1 0 0 1 72 656 Tm (Skiffia) Tj 1 0 0 1 260 656 Tm (francesae) Tj \
             1 0 0 1 72 560 Tm (Prose between the two tables sits here) Tj \
             1 0 0 1 72 460 Tm (Year) Tj 1 0 0 1 260 460 Tm (Event) Tj \
             1 0 0 1 72 436 Tm (2019) Tj 1 0 0 1 260 436 Tm (survey) Tj \
             1 0 0 1 72 416 Tm (2020) Tj 1 0 0 1 260 416 Tm (recovery) Tj ET",
        );
        assert!(
            md.contains("| Name | Kind |") && md.contains("| Year | Event |"),
            "both stacked tables detected: {md:?}"
        );
        assert!(
            md.contains("Prose between the two tables sits here") && !md.contains("| Prose"),
            "the prose between them stays prose: {md:?}"
        );
    }

    /// A lattice whose outer frame never made it into the rulings — only
    /// the interior verticals and the row rules are there. The rules'
    /// extents say where the frame was: the horizontals span the table's
    /// width, the verticals its height, and the outer columns and bands
    /// they imply hold the outer cells.
    #[test]
    fn interior_lattice_infers_its_outer_edges() {
        let md = markdown_of_drawn(
            "180 610 m 180 710 l S 258 610 m 258 710 l S \
             70 688 m 540 688 l S 70 648 m 540 648 l S \
             BT /F1 10 Tf 1 0 0 1 74 696 Tm (Channel) Tj 1 0 0 1 186 696 Tm (Medium) Tj 1 0 0 1 264 696 Tm (Examples) Tj \
             1 0 0 1 74 664 Tm (Direct) Tj 1 0 0 1 186 664 Tm (Physical) Tj 1 0 0 1 264 664 Tm (meetings) Tj \
             1 0 0 1 74 624 Tm (Indirect) Tj 1 0 0 1 186 624 Tm (Digital) Tj 1 0 0 1 264 624 Tm (websites) Tj ET",
        );
        assert!(
            md.contains("| Channel | Medium | Examples |"),
            "outer cells sit in inferred outer columns: {md:?}"
        );
        assert!(
            md.contains("| Direct | Physical | meetings |")
                && md.contains("| Indirect | Digital | websites |"),
            "all three bands row: {md:?}"
        );
    }

    /// A figure caption set larger than body text is a caption, not a
    /// heading: the marker word and number say so.
    #[test]
    fn a_large_figure_caption_is_not_a_heading() {
        let md = markdown_of(
            "BT /F1 13 Tf 72 700 Td (Figure 4.5. Breakdown of fuel by source) Tj ET \
             BT /F1 12 Tf 72 660 Td (Body text line one here for mass) Tj \
             0 -14 Td (Body text line two here for mass) Tj \
             0 -14 Td (Body text line three here for mass) Tj ET",
        );
        assert!(
            !md.contains("# Figure"),
            "a caption stays a caption: {md:?}"
        );
        assert!(md.contains("Figure 4.5. Breakdown of fuel by source"));
    }

    /// A table of contents sets every entry in heading-sized type; a run of
    /// same-level heading blocks with nothing between them is a list of
    /// entries, not document structure, and reads as plain text.
    #[test]
    fn a_run_of_same_level_headings_is_not_structure() {
        let entries: String = (1..=6)
            .map(|i| {
                format!(
                    "BT /F1 14 Tf 72 {} Td (Part {i}: A chapter title entry) Tj ET ",
                    720 - i * 40
                )
            })
            .collect();
        let body: String = (0..10)
            .map(|i| {
                format!(
                    "BT /F1 10 Tf 72 {} Td (A good long body line of ordinary prose text number {i}) Tj ET ",
                    440 - i * 12
                )
            })
            .collect();
        let md = markdown_of(&format!("{entries}{body}"));
        assert!(
            !md.contains("# Part 1"),
            "TOC entries are not headings: {md:?}"
        );
        assert!(md.contains("Part 1: A chapter title entry"));
    }

    /// A page whose first heading announces a table of contents keeps that
    /// one heading; the part titles below it are entries, however large
    /// they are set, even with numbered chapter lists between them.
    #[test]
    fn contents_page_part_titles_are_entries() {
        let md = markdown_of(
            "BT /F1 22 Tf 72 740 Td (Table of contents) Tj ET \
             BT /F1 14 Tf 72 700 Td (Part I: Different Toys 21) Tj ET \
             BT /F1 10 Tf 72 676 Td (3. The Child as Consumer 26) Tj \
             0 -14 Td (4. Domesticating Play 30) Tj 0 -14 Td (5. The Child in the City 35) Tj ET \
             BT /F1 14 Tf 72 600 Td (Part II: Networked Play 45) Tj ET \
             BT /F1 10 Tf 72 576 Td (7. LEGO Toys: from Blocks to Bricks 50) Tj \
             0 -14 Td (8. Brand Extension 58) Tj 0 -14 Td (9. Bringing the Fans in 62) Tj ET",
        );
        assert!(
            md.contains("# Table of contents"),
            "the page title stays a heading: {md:?}"
        );
        assert!(
            !md.contains("# Part I") && !md.contains("# Part II"),
            "entries are not headings: {md:?}"
        );
    }

    /// A drawn 2x2 grid leaves one lane, which the lane gates can never
    /// admit; the rulings alone make it a table.
    /// A statement rules each section's rows and leaves the section label
    /// between them unruled: the stacked boxes are one table, the label a
    /// row of it, the column heads above the top rule its header row, and
    /// the label and the amount sharing the first box two columns.
    #[test]
    fn stacked_section_boxes_are_one_table_with_their_header() {
        let md = markdown_of_drawn(&structure::tests::stacked_statement_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            1,
            "one table: {md}"
        );
        assert!(
            md.contains(
                "| Item |  | 2024 |\n| --- | --- | --- |\n| Cash | 1,000 | 5 |\n| Debt | 2,000 | 6 |\n\
                 | Capital |  |  |\n| Stock | 3,000 | 7 |\n| Total | 6,000 | 18 |"
            ),
            "md: {md}"
        );
    }

    /// Two boxed grids on the same verticals with a line of prose between
    /// them running across the column rule: two tables with the prose
    /// between them, not one table with the prose as a row, and not none.
    #[test]
    fn prose_between_two_boxes_keeps_them_two_tables() {
        let md = markdown_of_drawn(&structure::tests::boxes_with_prose_between_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            2,
            "two tables: {md}"
        );
        assert_eq!(md.matches("| Deductible | $500 |").count(), 2, "md: {md}");
        assert!(
            md.contains("B. Premium if the endorsement is attached to the policy."),
            "md: {md}"
        );
        assert!(!md.contains("| B. Premium"), "the prose is no row: {md}");
    }

    /// Two boxed grids on the same verticals with an empty gap of 220
    /// points between them are two tables.
    #[test]
    fn boxes_far_apart_are_two_tables() {
        let md = markdown_of_drawn(&structure::tests::boxes_far_apart_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            2,
            "two tables: {md}"
        );
    }

    /// Two boxed grids ten points apart on the same verticals, each
    /// opening with the same column heads, are two tables: a rate table
    /// repeats its heads over every section, a statement's sections never do.
    #[test]
    fn boxes_repeating_their_heads_stay_two_tables() {
        let md = markdown_of_drawn(&structure::tests::boxes_with_repeated_heads_content());
        assert_eq!(
            md.matches("| Area | Factor |\n| --- | --- |").count(),
            2,
            "md: {md}"
        );
    }

    /// A lower box whose head wraps a word onto a second line still repeats
    /// the head of the box above it, and the two stay two tables.
    #[test]
    fn a_wrapped_repeated_head_still_keeps_two_boxes_apart() {
        let md = markdown_of_drawn(&structure::tests::boxes_with_wrapped_repeated_heads_content());
        assert_eq!(
            md.lines().filter(|line| line.starts_with("| ---")).count(),
            2,
            "two tables: {md}"
        );
        assert_eq!(md.matches("| Area | Trading").count(), 2, "md: {md}");
    }

    /// A ruled band holding only a whitespace span is the page's padding:
    /// no row of blank cells appears between the two rows around it.
    #[test]
    fn a_blank_ruled_band_is_not_a_row() {
        let md = markdown_of_drawn(&structure::tests::ruled_blank_band_content());
        assert!(
            md.contains("| a1 | b1 |\n| --- | --- |\n| a2 | b2 |"),
            "md: {md}"
        );
        assert!(!md.contains("|  |  |"), "no blank row: {md}");
    }

    /// Text starting a few points left of the left border rule is the first
    /// column's, not a stray that fails the whole claim.
    #[test]
    fn text_overhanging_the_left_rule_keeps_the_claim() {
        let md = markdown_of_drawn(&structure::tests::overhanging_ruled_content());
        assert!(
            md.contains("| a1 | b1 |\n| --- | --- |\n| a2 | b2 |\n| a3 | b3 |"),
            "md: {md}"
        );
    }

    /// A word set glyph by glyph running across a vertical rule is one cell
    /// over both columns, and the grid keeps its claim.
    #[test]
    fn a_word_straddling_a_rule_glyph_by_glyph_keeps_the_claim() {
        let md = markdown_of_drawn(&structure::tests::glyph_straddle_ruled_content());
        assert!(md.contains("<td colspan=\"2\">swim</td>"), "md: {md}");
        assert!(
            md.contains("<td>the first cell of the top row</td><td>b1</td>"),
            "md: {md}"
        );
        assert!(md.contains("<td>a3</td><td>b3</td>"), "md: {md}");
    }

    /// A rule falling in the gap between two glyphs of one word leaves the
    /// word one cell over both columns, as it does when it runs through a
    /// glyph.
    #[test]
    fn a_rule_between_two_glyphs_of_a_word_keeps_the_word_whole() {
        let md = markdown_of_drawn(&structure::tests::glyph_gap_on_rule_content());
        assert!(md.contains("<td colspan=\"2\">swim</td>"), "md: {md}");
        assert!(md.contains("<td>a3</td><td>b3</td>"), "md: {md}");
    }

    /// A currency sign floating four points left of the vertical rule its
    /// amount stands behind belongs to the amount's column, not the label's.
    #[test]
    fn a_sign_a_hair_before_the_rule_belongs_to_its_amount() {
        let md = markdown_of_drawn(&structure::tests::sign_before_rule_content());
        assert!(
            md.contains("| Cash | $1,000 |\n| Debt | $2,000 |"),
            "md: {md}"
        );
    }

    /// The no-break spaces a producer pads a label with claim no column:
    /// the label's ink stops before the rule, and the amount behind the
    /// rule is its own cell.
    #[test]
    fn padding_inside_a_span_claims_no_column() {
        let md = markdown_of_drawn(&structure::tests::padded_label_ruled_content());
        assert!(
            md.contains("| Cash | 1,000 |\n| Debt | 2,000 |"),
            "md: {md}"
        );
    }

    #[test]
    fn a_ruled_grid_becomes_a_pipe_table() {
        let md = markdown_of_drawn(&structure::tests::ruled_grid_content());
        assert!(
            md.contains("| a1 | b1 |\n| --- | --- |\n| a2 | b2 |"),
            "md: {md}"
        );
    }

    /// A column whose head is set flush left and whose amounts flush right
    /// is one column: the lane between them is crossed by no line and
    /// splits nothing.
    #[test]
    fn a_lane_no_line_crosses_splits_no_drawn_column() {
        let md = markdown_of_drawn(&structure::tests::flush_head_flush_amount_ruled_content());
        assert!(
            md.contains("| Item | ALL |\n| --- | --- |\n| Cash | $37.43 |\n| Debt | $12.10 |"),
            "md: {md}"
        );
    }

    /// The corpus failure the ruled path fixes: a single-column boxed list
    /// is a one-column table, which no lane-occupancy gate could ever admit.
    #[test]
    fn a_single_column_boxed_list_becomes_a_table() {
        let md = markdown_of_drawn(&structure::tests::ruled_boxed_list_content());
        assert!(
            md.contains(
                "| first item |\n| --- |\n| second item |\n| third item |\n| fourth item |"
            ),
            "md: {md}"
        );
    }

    /// One ruled band holding three visual lines is one logical row: the
    /// wrapped cell's fragments join with single spaces and the other cells
    /// stay intact.
    #[test]
    fn a_wrapped_band_merges_into_one_logical_row() {
        let md = markdown_of_drawn(&structure::tests::ruled_wrapped_band_content());
        assert!(
            md.contains("| h1 | h2 | h3 |\n| --- | --- | --- |\n| m1 | m2 | m3 |"),
            "md: {md}"
        );
        assert!(
            md.contains("| wrap one wrap two wrap three | solo | tail |"),
            "the band's lines merge into one row: {md}"
        );
        assert!(
            !md.contains("| wrap two |"),
            "no fragmentary row survives: {md}"
        );
    }

    /// A grid ruled only on its interior boundaries: the header band above
    /// the top horizontal is claimed via the verticals' reach, the text
    /// overflowing the outer verticals opens a column on each side, and the
    /// rule-less data band's lines become one row per anchor line.
    #[test]
    fn an_open_edged_grid_becomes_a_full_table() {
        let md = markdown_of_drawn(&structure::tests::ruled_open_grid_content());
        assert!(
            md.contains(
                "| name | count | note |\n| --- | --- | --- |\n\
                 | alpha | one | xx |\n| beta | two | yy |\n\
                 | gamma | three | zz |\n| delta | four | ww |"
            ),
            "md: {md}"
        );
    }

    /// Records wrapping inside a rule-less band fold behind their anchor
    /// lines: the continuation populates no anchor cell, so it is the same
    /// row still being written, not a row of its own.
    #[test]
    fn wrapped_records_fold_behind_their_anchors() {
        let md = markdown_of_drawn(&structure::tests::ruled_wrapped_records_content());
        assert!(
            md.contains(
                "| name | org | count |\n| --- | --- | --- |\n\
                 | one | recordaa wrapa | c1 |\n| two | recordbb wrapb | c2 |"
            ),
            "md: {md}"
        );
    }

    /// A band between two rules whose lines are all figure records holds
    /// one row per line, however few lines the band has next to the whole
    /// claim: a timetable rules every few rows, not every row.
    #[test]
    fn banded_figure_records_are_rows() {
        let md = markdown_of_drawn(&structure::tests::ruled_banded_records_content());
        assert!(
            md.contains(
                "| South | Times | Bronx |\n| --- | --- | --- |\n\
                 | 12:00 | 12:04 | 12:17 |\n| 12:32 | 12:36 | 12:49 |\n\
                 | 1:14 | 1:18 | 1:31 |\n| 2:54 | 2:58 | 3:11 |\n| 3:00 | 3:04 | 3:17 |"
            ),
            "md: {md}"
        );
    }

    /// A rule-less band whose first line populates a single cell holds one
    /// vertically centered record: it merges whole instead of shattering at
    /// its anchor column.
    #[test]
    fn a_centered_record_band_merges_whole() {
        let md = markdown_of_drawn(&structure::tests::ruled_centered_record_content());
        assert!(
            md.contains(
                "| name | org | count |\n| --- | --- | --- |\n\
                 | actlinea actlineb actlinec actlined | union | c9 |"
            ),
            "md: {md}"
        );
    }

    /// A drawn grid no longer claims its whole segment: the whitespace-laned
    /// rows below it still become a table of their own — exactly the table
    /// the lane path alone emits — and blocks stay in reading order.
    #[test]
    fn a_ruled_grid_and_a_lane_grid_share_a_segment() {
        let content = structure::tests::ruled_grid_above_lane_grid_content();
        let md = markdown_of_drawn(&content);
        assert!(
            md.contains("| a1 | b1 |\n| --- | --- |\n| a2 | b2 |"),
            "the drawn grid stays a table: {md}"
        );
        let lane_table = [
            "| r0c0 | r0c1 | r0c2 |",
            "| --- | --- | --- |",
            "| r1c0 | r1c1 | r1c2 |",
            "| r2c0 | r2c1 | r2c2 |",
            "| r3c0 | r3c1 | r3c2 |",
        ]
        .join("\n");
        assert!(
            md.contains(&lane_table),
            "the laned rows stay a table: {md}"
        );
        assert!(
            md.find("| a1 |").unwrap() < md.find("| r0c0 |").unwrap(),
            "reading order: {md}"
        );
        assert!(
            markdown_of(&content).contains(&lane_table),
            "the lane path alone emits the same table"
        );
    }

    /// A grid boundary inside a sub-word gap never splits a word the flat
    /// flow writes whole: the word is one cell over both columns.
    #[test]
    fn a_ruling_inside_a_sub_word_gap_keeps_the_word_whole() {
        let md = markdown_of_drawn(&structure::tests::ruled_sub_word_gap_content());
        assert!(
            md.contains("<td colspan=\"2\">world</td>"),
            "one cell: {md}"
        );
        assert!(!md.contains("worl</td>"), "the word survives whole: {md}");
    }

    /// The spans-only entry points delegate with no rulings: a page whose
    /// only table is drawn stays prose through them, exactly as before.
    #[test]
    fn spans_only_layout_ignores_drawn_grids() {
        let doc = Document::load(doc_with_graphics(
            &structure::tests::ruled_boxed_list_content(),
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let (spans, report) =
            pdfboss_text::extract_spans_reporting(&doc, &page, ReadingOrder::Content).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        let layout = page_layout(&spans, ReadingOrder::Content);
        assert!(
            layout
                .blocks
                .iter()
                .all(|block| !matches!(block, Block::Table { .. })),
            "no rulings, no table: {layout:?}"
        );
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
        let pages: Vec<(Vec<TextSpan>, ReadingOrder)> = (0..2)
            .map(|i| {
                let page = doc.page(i).unwrap();
                let (spans, _) =
                    pdfboss_text::extract_spans_reporting(&doc, &page, ReadingOrder::Content)
                        .unwrap();
                (spans, ReadingOrder::Content)
            })
            .collect();
        let md = Markdown.render(&document_layout(&pages));
        assert!(
            md.contains("# Chapter Two"),
            "doc stats make it a heading: {md}"
        );
        let alone = Markdown.render(&[page_layout(&pages[1].0, ReadingOrder::Content)]);
        assert!(
            !alone.contains("# "),
            "page stats alone see 24pt as body: {alone}"
        );
    }

    /// A producer that marks its running head and page number as pagination
    /// artifacts needs no repetition across pages: on a single page they
    /// leave the Markdown and stay in the text, as the heuristic-tagged
    /// ones do.
    // Covers ISO 32000-1 §14.8.2.2.
    #[test]
    fn tagged_pagination_artifacts_are_page_headers_and_footers_on_one_page() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 9 0 R >> >> /Contents 6 0 R >>",
        );
        b.stream(
            6,
            "",
            b"BT /F1 10 Tf 72 770 Td /Artifact << /Type /Pagination /Subtype /Header >> BDC \
              (ACME REPORT) Tj EMC \
              /F1 12 Tf 0 -50 Td (Body text of the only page.) Tj \
              0 -14 Td (A second body line pads the page.) Tj \
              /F1 10 Tf 200 -666 Td /Artifact << /Type /Pagination /Subtype /Footer >> BDC \
              (7) Tj EMC ET",
        );
        b.object(
            9,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, _) =
            pdfboss_text::extract_spans_reporting(&doc, &page, ReadingOrder::Content).unwrap();
        let layouts = document_layout(&[(spans, ReadingOrder::Content)]);
        let md = Markdown.render(&layouts);
        assert!(!md.contains("ACME REPORT"), "md: {md}");
        assert!(!md.contains('7'), "page number dropped: {md}");
        assert!(
            md.contains("Body text of the only page"),
            "body survives: {md}"
        );
        let text = Text.render(&layouts);
        assert!(text.contains("ACME REPORT"), "text keeps the head: {text}");
        assert!(text.contains('7'), "text keeps the folio: {text}");
    }

    /// A running title repeated at the top of every page and a page number at
    /// the bottom disappear from markdown but stay in text.
    #[test]
    fn running_headers_and_page_numbers_are_tagged() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
        for (page_obj, contents_obj) in [(3u32, 6u32), (4, 7), (5, 8)] {
            b.object(
                page_obj,
                &format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Resources << /Font << /F1 9 0 R >> >> /Contents {contents_obj} 0 R >>"
                ),
            );
        }
        for (contents_obj, n) in [(6u32, 1u32), (7, 2), (8, 3)] {
            b.stream(
                contents_obj,
                "",
                format!(
                    "BT /F1 10 Tf 72 770 Td (ACME REPORT) Tj \
                     /F1 12 Tf 0 -50 Td (Page {n} body text differs everywhere.) Tj \
                     0 -14 Td (A second body line pads the page.) Tj \
                     /F1 10 Tf 200 -666 Td ({n}) Tj ET"
                )
                .as_bytes(),
            );
        }
        b.object(
            9,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let pages: Vec<(Vec<TextSpan>, ReadingOrder)> = (0..3)
            .map(|i| {
                let page = doc.page(i).unwrap();
                let (spans, _) =
                    pdfboss_text::extract_spans_reporting(&doc, &page, ReadingOrder::Content)
                        .unwrap();
                (spans, ReadingOrder::Content)
            })
            .collect();
        let layouts = document_layout(&pages);
        let md = Markdown.render(&layouts);
        assert!(!md.contains("ACME REPORT"), "md: {md}");
        assert!(!md.contains("\n1\n"), "page number dropped: {md}");
        assert!(md.contains("body text differs"), "body survives: {md}");
        let text = Text.render(&layouts);
        assert!(
            text.contains("ACME REPORT"),
            "text keeps everything: {text}"
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
        let (text, report) = extract_text_reporting(&doc, &page, ReadingOrder::Content).unwrap();
        assert_eq!(text, "", "the passthrough bytes must not be parsed");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::PageContents,
                cause: SkipCause::UnsupportedFilter("JPXDecode".to_string()),
            }],
        );
        // The plain entry point is the same leniency without the report.
        assert_eq!(
            extract_text(&doc, &page, ReadingOrder::Content).unwrap(),
            ""
        );
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
        let (text, report) = extract_text_reporting(&doc, &page, ReadingOrder::Content).unwrap();
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
        let (text, report) = extract_text_reporting(&doc, &page, ReadingOrder::Content).unwrap();
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
        let (text, report) = extract_text_reporting(&doc, &page, ReadingOrder::Content).unwrap();
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
        let (text, report) = extract_text_reporting(&doc, &page, ReadingOrder::Content).unwrap();
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
        let (text, report) = extract_text_reporting(&doc, &page, ReadingOrder::Content).unwrap();
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
                None,
                None,
                ReadingOrder::Content,
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

    /// A 300 by 200 page whose content stream also draws text far to the
    /// right of the page box — the pasteboard leftovers a cropped export
    /// keeps. `(inside)` sits on the page; `(pasteboard)` starts at x=650.
    fn pasteboard_doc() -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"BT /F1 12 Tf 50 100 Td (inside) Tj 600 0 Td (pasteboard) Tj ET",
        );
        b.object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        Document::load(b.build(1)).unwrap()
    }

    #[test]
    fn extraction_clips_to_the_page_box() {
        let doc = pasteboard_doc();
        assert_eq!(page_text(&doc, 0), "inside");
        let md = extract_markdown(&doc, ReadingOrder::Content).unwrap();
        assert!(md.contains("inside"), "markdown lost page text: {md:?}");
        assert!(
            !md.contains("pasteboard"),
            "markdown kept off-page text: {md:?}"
        );
    }

    /// A body-size space span drawn on the heading's baseline (a producer's
    /// stray separator) must not drag the line's size rank down to body: a
    /// space has no visible size, so it has no vote.
    #[test]
    fn stray_space_span_does_not_unrank_a_heading() {
        let md = markdown_of(
            "BT /F1 16 Tf 72 700 Td (Chapter One) Tj /F1 12 Tf ( ) Tj ET \
             BT /F1 12 Tf 72 660 Td (Body text line one here for mass) Tj \
             0 -14 Td (Body text line two here for mass) Tj \
             0 -14 Td (Body text line three here for mass) Tj ET",
        );
        assert!(
            md.contains("# Chapter One"),
            "heading lost to a stray space span: {md:?}"
        );
    }

    /// A small-caps heading sets word-initial capitals large and the rest
    /// of the capitals below body size. The line is all capitals in exactly
    /// two sizes — that is the small-caps signature — so it measures by its
    /// capital size, not by the small caps that would otherwise disqualify
    /// it.
    #[test]
    fn small_caps_heading_measures_by_its_capitals() {
        let md = markdown_of(
            "BT /F1 14 Tf 72 700 Td (R) Tj /F1 11 Tf (ECOLLECTION) Tj \
             /F1 14 Tf ( N) Tj /F1 11 Tf (OTES) Tj ET \
             BT /F1 12 Tf 72 660 Td (Body text line one here for mass) Tj \
             0 -14 Td (Body text line two here for mass) Tj \
             0 -14 Td (Body text line three here for mass) Tj ET",
        );
        assert!(
            md.contains("# R"),
            "small-caps heading lost its rank: {md:?}"
        );
    }

    /// Three sizes on one all-caps line is not small caps: it ranks by its
    /// smallest text like any other line (and must not panic, which the
    /// first cut of the two-bucket scan did on exactly this shape).
    #[test]
    fn three_sizes_on_a_line_rank_by_the_smallest() {
        let md = markdown_of(
            "BT /F1 14 Tf 72 700 Td (A) Tj /F1 11 Tf (BC) Tj /F1 12 Tf (DE) Tj /F1 11 Tf (FG) Tj ET \
             BT /F1 12 Tf 72 660 Td (Body text line one here for mass) Tj \
             0 -14 Td (Body text line two here for mass) Tj \
             0 -14 Td (Body text line three here for mass) Tj ET",
        );
        assert!(
            !md.contains("# A"),
            "a three-size line is not a small-caps heading: {md:?}"
        );
    }

    #[test]
    fn invisible_text_keeps_off_page_content() {
        let doc = pasteboard_doc();
        let page = doc.page(0).unwrap();
        let opts = TextOptions {
            invisible_text: true,
        };
        let (text, _) =
            extract_text_reporting_opts(&doc, &page, ReadingOrder::Content, opts).unwrap();
        assert_eq!(text, "inside pasteboard");
        let md = extract_markdown_opts(&doc, ReadingOrder::Content, opts).unwrap();
        assert!(
            md.contains("pasteboard"),
            "flag dropped off-page text: {md:?}"
        );
    }
}
