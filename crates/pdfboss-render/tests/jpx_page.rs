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
use pdfboss_render::{render_page_reporting, Pixmap, RenderOptions, RenderReport, SkippedKind};
use pdfboss_testkit::PdfBuilder;

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

/// Renders page 0 of `bytes` at one point per pixel, returning the pixmap
/// and the report for the caller to judge.
fn render_reported(bytes: &[u8]) -> (Pixmap, RenderReport) {
    let doc = Document::load(bytes.to_vec()).expect("the probe PDF opens");
    let page = doc.page(0).expect("the probe has one page");
    render_page_reporting(&doc, &page, 1.0, &RenderOptions::default()).expect("the page renders")
}

/// The embedded JP2 file of a wrapper fixture: the bytes from the JP2
/// signature box (T.800 I.5.1) to the `endstream` keyword, with the
/// end-of-line the writer put before the keyword stripped.
fn jp2_payload(pdf: &[u8]) -> Vec<u8> {
    let sig = [
        0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
    ];
    let start = pdf
        .windows(sig.len())
        .position(|w| w == sig)
        .expect("the fixture embeds a JP2 signature box");
    let end = start
        + pdf[start..]
            .windows(b"endstream".len())
            .position(|w| w == b"endstream")
            .expect("the stream ends");
    let mut data = pdf[start..end].to_vec();
    while matches!(data.last(), Some(b'\n' | b'\r')) {
        data.pop();
    }
    data
}

/// A one-page PDF drawing a single `/JPXDecode` image XObject named
/// `/Im0` under the given content stream; `dict_extra` lands in the
/// image dictionary (`/ColorSpace`, `/ImageMask`, ...).
fn jpx_probe_pdf_with_content(
    width: u32,
    height: u32,
    dict_extra: &str,
    content: &str,
    payload: &[u8],
) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        &format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
             /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>"
        ),
    );
    b.stream(4, "", content.as_bytes());
    b.stream(
        5,
        &format!(
            "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
             {dict_extra} /Filter /JPXDecode"
        ),
        payload,
    );
    b.build(1)
}

/// [`jpx_probe_pdf_with_content`] drawing the image over the whole page
/// at one point per pixel.
fn jpx_probe_pdf(width: u32, height: u32, dict_extra: &str, payload: &[u8]) -> Vec<u8> {
    jpx_probe_pdf_with_content(
        width,
        height,
        dict_extra,
        &format!("q {width} 0 0 {height} 0 0 cm /Im0 Do Q"),
        payload,
    )
}

/// One marker segment: the marker code, then Lmar = payload + 2 (T.800
/// A.1.2).
fn marker_segment(marker: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = marker.to_be_bytes().to_vec();
    v.extend(u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    v.extend_from_slice(payload);
    v
}

/// A minimal raw codestream (T.800 Annex A): one `size` x `size` tile,
/// one unsigned `depth`-bit component, no wavelet decomposition (NL = 0),
/// and a single EMPTY packet (B.10.3: a first packet-header bit of 0).
/// Every coefficient is zero, so each decoded sample is exactly the G.1.2
/// level shift `2^(depth - 1)` — a uniform image whose one known sample
/// value the tests below reason from, with no entropy-coded data to
/// hand-encode.
fn uniform_codestream(size: u32, depth: u8) -> Vec<u8> {
    const SOC: u16 = 65359; // 0xFF4F (Table A.4)
    const SIZ: u16 = 65361; // 0xFF51 (Table A.9)
    const COD: u16 = 65362; // 0xFF52 (Table A.12)
    const QCD: u16 = 65372; // 0xFF5C (Table A.27)
    const SOT: u16 = 65424; // 0xFF90 (Table A.5)
    const SOD: u16 = 65427; // 0xFF93 (Table A.7)
    const EOC: u16 = 65497; // 0xFFD9 (Table A.8)
    let mut v = SOC.to_be_bytes().to_vec();
    // SIZ (Table A.9): zero offsets, one tile covering the whole grid,
    // one unsigned component (Ssiz = depth - 1) at unit separation.
    let mut siz = 0u16.to_be_bytes().to_vec(); // Rsiz
    for value in [size, size, 0, 0, size, size, 0, 0] {
        siz.extend(value.to_be_bytes());
    }
    siz.extend(1u16.to_be_bytes()); // Csiz
    siz.extend([depth - 1, 1, 1]);
    v.extend(marker_segment(SIZ, &siz));
    // COD (Figure A.9): Scod = 0 (default 2^15 precincts), LRCP, 1 layer,
    // no MCT, NL = 0, 64x64 code-blocks (signalled xcb - 2 = 4), block
    // style 0, reversible 5-3 wavelet.
    v.extend(marker_segment(COD, &[0, 0, 0, 1, 0, 0, 4, 4, 0, 1]));
    // QCD (Table A.28): Sqcd = 64 (no quantization, 2 guard bits), one
    // exponent byte for the single NL = 0 sub-band.
    v.extend(marker_segment(QCD, &[64, depth << 3]));
    // One tile-part running to EOC (Psot = 0, A.4.2).
    let mut sot = 0u16.to_be_bytes().to_vec();
    sot.extend(0u32.to_be_bytes());
    sot.extend([0, 1]); // TPsot = 0, TNsot = 1
    v.extend(marker_segment(SOT, &sot));
    v.extend(SOD.to_be_bytes());
    // The tile's one packet (1 layer x 1 resolution x 1 precinct), empty.
    v.push(0);
    v.extend(EOC.to_be_bytes());
    v
}

fn probe_px(pix: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    let off = ((y * pix.width + x) * 4) as usize;
    pix.data[off..off + 4].try_into().unwrap()
}

/// `/ImageMask true` over a JPXDecode stream is a stencil (ISO 32000-1
/// 7.4.9: the codestream provides "a single colour channel with 1-bit
/// samples"): it paints the CURRENT FILL COLOUR where the mapped sample
/// is 0 and leaves the page untouched elsewhere — never its own gray.
/// The uniform 1-bit stream decodes every sample to 1, so `/Decode [1 0]`
/// (which 7.4.9 keeps FOR masks) maps them to painting: the whole page
/// must come out red, not the white a sample of 1 would paint if the
/// image were (wrongly) drawn in its own colours.
#[test]
fn an_imagemask_jpx_stencils_the_fill_color() {
    let pdf = jpx_probe_pdf_with_content(
        8,
        8,
        "/ImageMask true /BitsPerComponent 1 /Decode [1 0]",
        "1 0 0 rg q 8 0 0 8 0 0 cm /Im0 Do Q",
        &uniform_codestream(8, 1),
    );
    let (pix, report) = render_reported(&pdf);
    assert!(
        report.is_empty(),
        "nothing to skip: {:?}",
        report.warnings()
    );
    assert_eq!(probe_px(&pix, 4, 4), [255, 0, 0, 255], "fill paints");
    assert_eq!(probe_px(&pix, 0, 7), [255, 0, 0, 255], "everywhere");
}

/// The same stencil under the DEFAULT `/Decode [0 1]`: every sample maps
/// to 1, which a stencil leaves unpainted, so the page stays white. This
/// is the other half of `/Decode` applying to masks: the two arrays must
/// paint opposite pages.
#[test]
fn an_imagemask_jpx_honors_the_default_decode() {
    let pdf = jpx_probe_pdf_with_content(
        8,
        8,
        "/ImageMask true /BitsPerComponent 1",
        "1 0 0 rg q 8 0 0 8 0 0 cm /Im0 Do Q",
        &uniform_codestream(8, 1),
    );
    let (pix, report) = render_reported(&pdf);
    assert!(
        report.is_empty(),
        "nothing to skip: {:?}",
        report.warnings()
    );
    assert_eq!(probe_px(&pix, 4, 4), [255, 255, 255, 255], "untouched");
}

/// A multi-channel codestream under `/ImageMask true` is malformed (7.4.9
/// demands a single channel): the image is skipped with a report entry,
/// not painted in its own colours.
#[test]
fn a_multichannel_jpx_under_imagemask_is_skipped_with_a_report_entry() {
    let payload = jp2_payload(include_bytes!("fixtures/pdf-rgb-53.pdf"));
    let pdf = jpx_probe_pdf_with_content(
        130,
        83,
        "/ImageMask true /BitsPerComponent 1",
        "1 0 0 rg q 130 0 0 83 0 0 cm /Im0 Do Q",
        &payload,
    );
    let (pix, report) = render_reported(&pdf);
    assert_eq!(nonwhite(&pix), 0, "nothing painted");
    assert!(
        report.skipped.iter().any(|item| {
            item.kind == SkippedKind::Image && item.reason.to_string().contains("ImageMask")
        }),
        "the malformed stencil must be a report entry: {:?}",
        report.warnings()
    );
}

/// A corrupt-but-decodable codestream must still paint AND own up to the
/// loss: eight bytes near the end of the tile body are flipped, which
/// garbles a highest-resolution packet while every earlier packet decodes
/// (the decoder's leniency doctrine), so most of the image paints and the
/// decoder attaches one data-loss warning that must surface as a report
/// entry rather than pass the render off as faithful.
#[test]
fn a_corrupted_jpx_payload_paints_and_reports_the_loss() {
    let mut pdf = include_bytes!("fixtures/pdf-rgb-53.pdf").to_vec();
    // Flip payload bytes in place (the length is unchanged, so the PDF
    // stays consistent): 90% into the tile body, past the SOD marker
    // (T.800 A.4.3).
    let payload_len = jp2_payload(&pdf).len();
    let payload_start = pdf
        .windows(12)
        .position(|w| {
            w == [
                0x00, 0x00, 0x00, 0x0C, 0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
            ]
        })
        .expect("signature");
    let payload_end = payload_start + payload_len;
    let sod = payload_start
        + pdf[payload_start..payload_end]
            .windows(2)
            .position(|w| w == [0xFF, 0x93])
            .expect("SOD marker");
    // The tile body runs from past SOD to the EOC marker's two bytes.
    let body_len = payload_end - (sod + 2) - 2;
    let at = sod + 2 + body_len * 9 / 10;
    for byte in &mut pdf[at..at + 8] {
        *byte ^= 0xFF;
    }

    let (pix, report) = render_reported(&pdf);
    let total = (pix.width * pix.height) as usize;
    let inked = nonwhite(&pix);
    assert!(
        inked > total / 2,
        "{inked} of {total} pixels have ink; leniency should still paint"
    );
    assert!(
        report.skipped.iter().any(|item| {
            item.kind == SkippedKind::Image && item.reason.to_string().contains("JPXDecode")
        }),
        "the data loss must be a report entry: {:?}",
        report.warnings()
    );
}

/// Benign decoder notes (`data_loss: false`) must NOT dirty the report:
/// stripping the EOC marker (with the jp2c box length shrunk to match)
/// decodes every sample — the body already ran to its end — and leaves
/// only the advisory "missing EOC (A.4.4)" note behind, so the render
/// reports clean.
#[test]
fn a_benign_decoder_note_keeps_the_report_clean() {
    let mut payload = jp2_payload(include_bytes!("fixtures/pdf-rgb-53.pdf"));
    assert_eq!(&payload[payload.len() - 2..], &[0xFF, 0xD9], "EOC last");
    payload.truncate(payload.len() - 2);
    let jp2c = payload
        .windows(4)
        .position(|w| w == b"jp2c")
        .expect("jp2c box");
    let len_at = jp2c - 4;
    let old = u32::from_be_bytes(payload[len_at..len_at + 4].try_into().unwrap());
    payload[len_at..len_at + 4].copy_from_slice(&(old - 2).to_be_bytes());

    let pdf = jpx_probe_pdf(
        130,
        83,
        "/ColorSpace /DeviceRGB /BitsPerComponent 8",
        &payload,
    );
    let (pix, report) = render_reported(&pdf);
    let total = (pix.width * pix.height) as usize;
    let inked = nonwhite(&pix);
    assert!(inked > total / 2, "{inked} of {total} pixels have ink");
    assert!(
        report.is_empty(),
        "a benign note is not a drop: {:?}",
        report.warnings()
    );
}
