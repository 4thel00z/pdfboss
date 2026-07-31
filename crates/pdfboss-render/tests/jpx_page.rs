//! End-to-end coverage of `JPXDecode` (JPEG 2000, ISO 32000-1 7.4.9)
//! images, from PDF bytes to rasterized pixels.
//!
//! The unit tests inside `pdfboss-jpx` check the codec against its own
//! fixture zoo, which pins the decoder to ITU-T T.800 but cannot notice a
//! break anywhere else on the path: the filter chain has to hand the
//! codestream through untouched, the image layer has to recognize it, and
//! the decoded samples have to survive colorspace conversion and
//! compositing. These tests walk that whole path: a PDF file, opened
//! through `Document`, its page rasterized through
//! `render_page_reporting`, and the pixels judged.
//!
//! The fixtures are synthetic wrappers built for this repository: each
//! embeds one JP2 file (produced from a synthetic source image) in an
//! image XObject drawn to fill the page at one point per pixel. The names
//! carry the wavelet: `-53` is the reversible 5-3 filter, `-97` the
//! irreversible 9-7 (T.800 Annex F).
//!
//! As in the JBIG2 sibling test, the assertions are about ink rather than
//! about `Ok`: a render that skips the image still returns `Ok` and a
//! blank page, and only the report and the pixel counts tell those
//! outcomes apart.

use pdfboss_core::Document;
use pdfboss_render::{render_page_reporting, Pixmap, RenderOptions};

/// Renders page 0 of `bytes` at one point per pixel, asserting that
/// nothing was skipped or approximated: a JPX image the rasterizer could
/// not decode is a report entry, not an error.
fn render_clean(bytes: &[u8]) -> Pixmap {
    let doc = Document::load(bytes.to_vec()).expect("the fixture PDF opens");
    let page = doc.page(0).expect("the fixture has one page");
    let (pix, report) = render_page_reporting(&doc, &page, 1.0, &RenderOptions::default())
        .expect("the page renders");
    assert!(
        report.is_empty(),
        "content was skipped: {:?}",
        report.warnings()
    );
    pix
}

/// How many pixels are visibly not the white background (any channel
/// below 245 — the 9-7 wavelet is lossy, so near-white is still white).
fn nonwhite(pix: &Pixmap) -> usize {
    pix.data
        .chunks_exact(4)
        .filter(|px| px[0] < 245 || px[1] < 245 || px[2] < 245)
        .count()
}

/// How many pixels are visibly chromatic rather than a shade of gray.
fn colored(pix: &Pixmap) -> usize {
    pix.data
        .chunks_exact(4)
        .filter(|px| {
            let hi = px[0].max(px[1]).max(px[2]);
            let lo = px[0].min(px[1]).min(px[2]);
            hi - lo > 32
        })
        .count()
}

/// A 130x83 `/DeviceRGB` image over a reversible 5-3 codestream: the
/// source image has no white pixels at all and is chromatic nearly
/// everywhere, so a blank page, a gray misread of the interleave, and a
/// skipped image all land far below the thresholds.
#[test]
fn a_jpx_rgb_image_renders_in_color_with_no_skips() {
    let pix = render_clean(include_bytes!("fixtures/pdf-rgb-53.pdf"));
    assert_eq!((pix.width, pix.height), (130, 83));
    let total = (pix.width * pix.height) as usize;
    let inked = nonwhite(&pix);
    assert!(
        inked > total / 2,
        "{inked} of {total} pixels have ink; the image did not paint"
    );
    let chroma = colored(&pix);
    assert!(
        chroma > total / 2,
        "{chroma} of {total} pixels are chromatic; RGB decoded as something else"
    );
}

/// A 97x61 `/DeviceGray` image over an irreversible 9-7 codestream: the
/// source is nearly all mid-tones, so the ink fraction separates a decoded
/// page from a blank one even under the wavelet's small sample errors.
#[test]
fn a_jpx_gray_image_renders_with_no_skips() {
    let pix = render_clean(include_bytes!("fixtures/pdf-gray-97.pdf"));
    assert_eq!((pix.width, pix.height), (97, 61));
    let total = (pix.width * pix.height) as usize;
    let inked = nonwhite(&pix);
    assert!(
        inked > total / 2,
        "{inked} of {total} pixels have ink; the image did not paint"
    );
}
