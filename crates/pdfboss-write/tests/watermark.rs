//! Watermarking as an incremental update: the base file's bytes stay
//! untouched at the front, an update section appends the overlay page as a
//! form drawn over every page, and pdfboss-core reads the result back with
//! both texts on every page.

use pdfboss_core::Document;
use pdfboss_output::extract_text;
use pdfboss_render::{render_page_reporting, RenderOptions};
use pdfboss_write::{watermark, Page, PageSize, Pdf, Standard14, WriteOptions, XrefStyle};

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
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("DRAFT", 200.0, 400.0, Standard14::HelveticaBold, 48.0)
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
        let text = extract_text(&doc, &page).unwrap();
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
