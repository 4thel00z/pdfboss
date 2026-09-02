//! Watermarking as an incremental update: the base file's bytes stay
//! untouched at the front, an update section appends the overlay page as a
//! form drawn over every page, and pdfboss-core reads the result back with
//! both texts on every page.

use pdfboss_core::Document;
use pdfboss_output::{extract_text, ReadingOrder};
use pdfboss_render::{render_page_reporting, RenderOptions};
use pdfboss_write::{
    watermark, watermark_under, watermark_under_with, watermark_with, Page, PageSize, Pdf,
    Standard14, WriteOptions, XrefStyle,
};

fn base_pdf(xref: XrefStyle) -> Vec<u8> {
    let mut first = Page::new(PageSize::A4);
    first
        .canvas
        .text("Base page one", 72.0, 700.0, Standard14::Helvetica, 14.0)
        .unwrap();
    let mut second = Page::new(PageSize::A4);
    second
        .canvas
        .text("Base page two", 72.0, 700.0, Standard14::TimesRoman, 14.0)
        .unwrap();
    Pdf {
        pages: vec![first, second],
        options: WriteOptions {
            xref,
            ..WriteOptions::default()
        },
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap()
}

fn overlay_pdf() -> Vec<u8> {
    overlay_pdf_with_text("DRAFT")
}

fn overlay_pdf_with_text(text: &str) -> Vec<u8> {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text(text, 200.0, 400.0, Standard14::HelveticaBold, 48.0)
        .unwrap();
    Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap()
}

fn assert_watermarked(base: Vec<u8>) {
    let base_doc = Document::load(base.clone()).unwrap();
    let overlay_doc = Document::load(overlay_pdf()).unwrap();
    let out = watermark(&base_doc, &overlay_doc).unwrap();
    assert!(
        out.starts_with(&base),
        "an update keeps the base bytes in place"
    );
    assert!(
        out.len() < base.len() + 4096,
        "an update adds only the overlay: {} bytes",
        out.len() - base.len()
    );

    let doc = Document::load(out).unwrap();
    assert!(
        doc.xref().trailer.get("Prev").is_some(),
        "the update section chains to the base xref"
    );
    assert_eq!(doc.page_count(), 2);
    for (index, expected) in ["Base page one", "Base page two"].iter().enumerate() {
        let page = doc.page(index).unwrap();
        let text = extract_text(&doc, &page, ReadingOrder::Content).unwrap();
        assert!(text.contains(expected), "page {index}: {text:?}");
        assert!(text.contains("DRAFT"), "page {index}: {text:?}");
        let (_, report) =
            render_page_reporting(&doc, &page, 1.0, &RenderOptions::default()).unwrap();
        assert!(
            report.is_empty(),
            "page {index} rendered with skips: {report:?}"
        );
    }
}

#[test]
fn watermark_updates_a_table_xref_file() {
    assert_watermarked(base_pdf(XrefStyle::Table));
}

#[test]
fn watermark_updates_a_stream_xref_file() {
    assert_watermarked(base_pdf(XrefStyle::Stream));
}

/// The rewrite variant writes a fresh file through the writer, so an
/// uncompressed base comes out smaller than it went in, with no `/Prev`
/// chain, and still carries both texts on every page.
#[test]
fn watermark_with_rewrites_the_file_compressed() {
    let mut first = Page::new(PageSize::A4);
    let mut second = Page::new(PageSize::A4);
    for line in 0..150 {
        let y = 780.0 - line as f32 * 5.0;
        first
            .canvas
            .text(
                "Base page one, a line of running text",
                72.0,
                y,
                Standard14::Helvetica,
                4.0,
            )
            .unwrap();
        second
            .canvas
            .text(
                "Base page two, a line of running text",
                72.0,
                y,
                Standard14::TimesRoman,
                4.0,
            )
            .unwrap();
    }
    let base = Pdf {
        pages: vec![first, second],
        options: WriteOptions {
            compress: false,
            object_streams: false,
            xref: XrefStyle::Table,
            ..WriteOptions::default()
        },
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let base_doc = Document::load(base.clone()).unwrap();
    let overlay_doc = Document::load(overlay_pdf()).unwrap();
    let out = watermark_with(&base_doc, &overlay_doc, WriteOptions::default()).unwrap();
    assert!(
        out.len() < base.len(),
        "{} bytes from a {} byte base",
        out.len(),
        base.len()
    );
    let doc = Document::load(out).unwrap();
    assert!(
        doc.xref().trailer.get("Prev").is_none(),
        "a rewrite has one section"
    );
    assert_eq!(doc.page_count(), 2);
    for (index, expected) in ["Base page one", "Base page two"].iter().enumerate() {
        let page = doc.page(index).unwrap();
        let text = extract_text(&doc, &page, ReadingOrder::Content).unwrap();
        assert!(text.contains(expected), "page {index}: {text:?}");
        assert!(text.contains("DRAFT"), "page {index}: {text:?}");
    }
}

/// Page 0's decoded content of a document loaded from `bytes`, as text.
fn page0_content_text(bytes: &[u8]) -> String {
    let doc = Document::load(bytes.to_vec()).unwrap();
    let page = doc.page(0).unwrap();
    String::from_utf8(page.content(&doc).unwrap()).unwrap()
}

#[test]
fn under_append_keeps_base_bytes() {
    let base = base_pdf(XrefStyle::Table);
    let base_doc = Document::load(base.clone()).unwrap();
    let overlay_doc = Document::load(overlay_pdf()).unwrap();
    let out = watermark_under(&base_doc, &overlay_doc).unwrap();
    assert!(
        out.starts_with(&base),
        "an under update keeps the base bytes in place"
    );
}

/// Over draws the form after the page's own content; under draws it
/// first, so the page's own content is the last thing drawn. Checked for
/// both the append path (`watermark`/`watermark_under`) and the rewrite
/// path (`watermark_with`/`watermark_under_with`).
#[test]
fn under_draws_before_the_content() {
    let base = base_pdf(XrefStyle::Table);
    let base_doc = Document::load(base).unwrap();
    let overlay_doc = Document::load(overlay_pdf()).unwrap();

    let over = watermark(&base_doc, &overlay_doc).unwrap();
    let over_content = page0_content_text(&over);
    assert!(
        over_content.trim_end().ends_with("/PdfbossWatermark Do Q"),
        "over draws the form after the page's own content: {over_content:?}"
    );
    assert!(
        !over_content.starts_with("q /PdfbossWatermark"),
        "over does not draw the form before the page's own content: {over_content:?}"
    );

    let under = watermark_under(&base_doc, &overlay_doc).unwrap();
    let under_content = page0_content_text(&under);
    assert!(
        under_content.starts_with("q /PdfbossWatermark Do Q"),
        "under draws the form before the page's own content: {under_content:?}"
    );
    assert!(
        !under_content.trim_end().ends_with("Do Q"),
        "under does not draw the form again after the page's own content: {under_content:?}"
    );

    let over_with = watermark_with(&base_doc, &overlay_doc, WriteOptions::default()).unwrap();
    let over_with_content = page0_content_text(&over_with);
    assert!(
        over_with_content
            .trim_end()
            .ends_with("/PdfbossWatermark Do Q"),
        "over_with draws the form after the page's own content: {over_with_content:?}"
    );
    assert!(
        !over_with_content.starts_with("q /PdfbossWatermark"),
        "over_with does not draw the form before the page's own content: {over_with_content:?}"
    );

    let under_with =
        watermark_under_with(&base_doc, &overlay_doc, WriteOptions::default()).unwrap();
    let under_with_content = page0_content_text(&under_with);
    assert!(
        under_with_content.starts_with("q /PdfbossWatermark Do Q"),
        "under_with draws the form before the page's own content: {under_with_content:?}"
    );
    assert!(
        !under_with_content.trim_end().ends_with("Do Q"),
        "under_with does not draw the form again after the page's own content: {under_with_content:?}"
    );
}

#[test]
fn under_text_survives() {
    let base = base_pdf(XrefStyle::Table);
    let base_doc = Document::load(base).unwrap();
    let overlay_doc = Document::load(overlay_pdf()).unwrap();
    let out = watermark_under(&base_doc, &overlay_doc).unwrap();

    let doc = Document::load(out).unwrap();
    assert_eq!(doc.page_count(), 2);
    for (index, expected) in ["Base page one", "Base page two"].iter().enumerate() {
        let page = doc.page(index).unwrap();
        let text = extract_text(&doc, &page, ReadingOrder::Content).unwrap();
        assert!(text.contains(expected), "page {index}: {text:?}");
        assert!(text.contains("DRAFT"), "page {index}: {text:?}");
    }
}

/// A second overlay on an already-marked file draws under its own free
/// name (`PdfbossWatermark2`) instead of replacing the first mark's
/// `/XObject` entry, so both draw operators survive and each fires once.
#[test]
fn overlaying_twice_keeps_both_marks() {
    let base_doc = Document::load(base_pdf(XrefStyle::Table)).unwrap();
    let first_overlay = Document::load(overlay_pdf_with_text("MARKONE")).unwrap();
    let once = watermark(&base_doc, &first_overlay).unwrap();

    let once_doc = Document::load(once).unwrap();
    let second_overlay = Document::load(overlay_pdf_with_text("MARKTWO")).unwrap();
    let twice = watermark(&once_doc, &second_overlay).unwrap();

    let content = page0_content_text(&twice);
    assert_eq!(
        content.matches("/PdfbossWatermark Do").count(),
        1,
        "first mark's draw operator fires exactly once: {content:?}"
    );
    assert_eq!(
        content.matches("/PdfbossWatermark2 Do").count(),
        1,
        "second mark's draw operator fires exactly once: {content:?}"
    );

    let doc = Document::load(twice).unwrap();
    let page = doc.page(0).unwrap();
    let text = extract_text(&doc, &page, ReadingOrder::Content).unwrap();
    assert!(text.contains("Base page one"), "page 0: {text:?}");
}
