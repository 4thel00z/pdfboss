//! End-to-end coverage of `/Separation` tint transforms (ISO 32000-1 8.6.6.4),
//! from PDF bytes to rasterized pixels.
//!
//! The unit tests build their spaces inline, which pins the conversion but not
//! the path a real file takes: the colour space has to be found in the page's
//! `/Resources`, its transform loaded from an indirect object, and the tint
//! carried from `sc` through to compositing. This walks that path over a file.
//!
//! The fixture is the 686-byte reproducer from #52: one page filled with a
//! `/Separation` whose type-2 transform maps full tint to a pale cream in
//! `DeviceCMYK`. The assertion is about the colour rather than about `Ok` — the
//! bug it covers rendered the page solid black and reported nothing wrong.

use pdfboss_core::Document;
use pdfboss_render::{render_page_reporting, RenderOptions};

#[test]
fn separation_fill_takes_its_color_from_the_tint_transform() {
    let bytes = include_bytes!("fixtures/separation-tint.pdf").to_vec();
    let doc = Document::load(bytes).expect("the fixture PDF opens");
    let page = doc.page(0).expect("the fixture has one page");
    let (pix, report) = render_page_reporting(&doc, &page, 1.0, &RenderOptions::default())
        .expect("the page renders");

    assert!(
        report.is_empty(),
        "content was skipped: {:?}",
        report.warnings()
    );
    let middle = ((pix.height / 2) * pix.width + pix.width / 2) as usize * 4;
    let pixel = &pix.data[middle..middle + 3];
    // tint 1 -> CMYK 0.02 0.012 0.129 0 -> a pale cream, not black.
    assert!(
        pixel[0] > 240 && pixel[1] > 240 && (210..240).contains(&pixel[2]),
        "expected the transform's pale cream, got {pixel:?}",
    );
}
