//! Extraction agrees with the writer about standard-14 advances: a line set
//! with `Text` in Times, Helvetica, or Courier reports an `end_x` where
//! `Standard14::text_width` puts it, which is where the painted ink ends.

use pdfboss_core::{Document, Point};
use pdfboss_text::{extract_spans, ReadingOrder};
use pdfboss_write::{Page, PageSize, Pdf, Standard14, Text};

const SAMPLE: &str =
    "The quick brown fox jumps over the lazy dog while the committee reviews the annual budget.";
const LEFT: f32 = 36.0;
const SIZE: f32 = 10.0;

/// One letter page with `SAMPLE` set at `(LEFT, 700)` in `font`.
fn one_line(font: Standard14) -> Vec<u8> {
    let text = Text {
        value: SAMPLE.to_string(),
        at: Point { x: LEFT, y: 700.0 },
        font,
        size: SIZE,
        ..Text::default()
    };
    let page = Page {
        size: PageSize::Letter,
        content: vec![text.into()],
        ..Page::default()
    };
    Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .expect("a one-line page serializes")
}

// Covers ISO 32000-1 §9.6.2.2.
#[test]
fn extracted_end_x_matches_the_writers_text_width() {
    for font in [
        Standard14::TimesRoman,
        Standard14::Helvetica,
        Standard14::Courier,
    ] {
        let doc = Document::load(one_line(font)).expect("load");
        let page = doc.page(0).expect("page");
        let spans = extract_spans(&doc, &page, ReadingOrder::Content).expect("spans");
        let end_x = spans.iter().map(|s| s.end_x).fold(f32::MIN, f32::max);
        let expected = LEFT + font.text_width(SAMPLE, SIZE).expect("AFM metrics");
        assert!(
            (end_x - expected).abs() < 0.05,
            "{font:?}: extracted end_x {end_x} vs writer end {expected}"
        );
    }
}
