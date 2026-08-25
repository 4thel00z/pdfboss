//! End-to-end coverage of `ICCBased` and `Lab` colour (ISO 32000-1 8.6.5),
//! from PDF bytes to rasterized pixels.
//!
//! The colour unit tests pin the conversions; these walk the real path — the
//! space found in the page's `/Resources`, the profile stream decoded, the
//! colour carried from `scn` or an image through to compositing — and pin
//! the two ends of the fast-path bargain: a file wrapping sRGB stays
//! byte-identical to plain DeviceRGB, and a genuinely different profile
//! moves the pixels by exactly the computed amount.

use pdfboss_core::Document;
use pdfboss_render::{render_page_reporting, Pixmap, RenderOptions};
use pdfboss_testkit::PdfBuilder;

fn fx(v: f64) -> [u8; 4] {
    (((v * 65536.0).round()) as i32).to_be_bytes()
}

/// A minimal matrix/TRC RGB profile: the sRGB colorant columns in the D50
/// PCS plus one shared TRC tag.
fn rgb_profile(trc: &[u8]) -> Vec<u8> {
    let columns: [[f64; 3]; 3] = [
        [0.4360, 0.2225, 0.0139],
        [0.3851, 0.7169, 0.0971],
        [0.1431, 0.0606, 0.7139],
    ];
    let mut tags: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for (sig, col) in [*b"rXYZ", *b"gXYZ", *b"bXYZ"].iter().zip(columns) {
        let mut data = b"XYZ \0\0\0\0".to_vec();
        for v in col {
            data.extend_from_slice(&fx(v));
        }
        tags.push((*sig, data));
    }
    for sig in [*b"rTRC", *b"gTRC", *b"bTRC"] {
        tags.push((sig, trc.to_vec()));
    }
    let mut header = vec![0u8; 128];
    header[8] = 4;
    header[16..20].copy_from_slice(b"RGB ");
    header[20..24].copy_from_slice(b"XYZ ");
    header[36..40].copy_from_slice(b"acsp");
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut body = Vec::new();
    let mut at = 132 + 12 * tags.len();
    for (sig, data) in &tags {
        table.extend_from_slice(sig);
        table.extend_from_slice(&(at as u32).to_be_bytes());
        table.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        at += data.len();
    }
    let mut out = header;
    out.extend_from_slice(&table);
    out.extend_from_slice(&body);
    let size = (out.len() as u32).to_be_bytes();
    out[0..4].copy_from_slice(&size);
    out
}

fn srgb_trc() -> Vec<u8> {
    let mut out = b"para\0\0\0\0\0\x03\0\0".to_vec();
    for v in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
        out.extend_from_slice(&fx(v));
    }
    out
}

fn gamma_trc(g: f64) -> Vec<u8> {
    let mut out = b"curv\0\0\0\0\0\0\0\x01".to_vec();
    out.extend_from_slice(&(((g * 256.0).round()) as u16).to_be_bytes());
    out
}

/// One 8x8 page filled through colour space `cs` (object 4 holds `profile`
/// when given) with the mid-gray `0.5 0.5 0.5 scn`.
fn fill_page(cs: &str, profile: Option<&[u8]>) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        &format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 8 8] \
             /Resources << /ColorSpace << /CS0 {cs} >> >> /Contents 5 0 R >>"
        ),
    );
    if let Some(p) = profile {
        b.stream(4, "/N 3", p);
    }
    b.stream(5, "", b"/CS0 cs 0.5 0.5 0.5 scn 0 0 8 8 re f");
    b.build(1)
}

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

/// An `ICCBased` fill through a profile that is sRGB renders byte-identical
/// to the same fill in plain `/DeviceRGB`.
#[test]
fn an_srgb_iccbased_fill_matches_device_rgb_exactly() {
    let device = render_clean(&fill_page("/DeviceRGB", None));
    let icc = render_clean(&fill_page(
        "[/ICCBased 4 0 R]",
        Some(&rgb_profile(&srgb_trc())),
    ));
    assert_eq!(device.data, icc.data, "the sRGB fast path must not repaint");
}

/// The same fill through a gamma-1,8 profile moves mid-gray to the sRGB
/// encoding of 0,5^1,8, computed here rather than transcribed.
#[test]
fn a_gamma_18_iccbased_fill_shifts_by_the_computed_amount() {
    let pix = render_clean(&fill_page(
        "[/ICCBased 4 0 R]",
        Some(&rgb_profile(&gamma_trc(1.8))),
    ));
    let lin = 0.5f32.powf(1.8);
    let encoded = 1.055 * lin.powf(1.0 / 2.4) - 0.055;
    let want = (encoded * 255.0 + 0.5) as u8;
    let middle = ((pix.height / 2) * pix.width + pix.width / 2) as usize * 4;
    let pixel = &pix.data[middle..middle + 3];
    for got in pixel {
        assert!(
            got.abs_diff(want) <= 1,
            "expected about {want} per channel, got {pixel:?}"
        );
    }
    let device = render_clean(&fill_page("/DeviceRGB", None));
    assert_ne!(device.data, pix.data, "a non-sRGB profile must repaint");
}

/// A 1x1 `/Lab` image with no `/Decode` reads its samples through the Lab
/// component ranges: bytes (255, 128, 128) decode to L* = 100 near the
/// whitepoint, not to a dark L* = 1.
#[test]
fn a_lab_image_defaults_its_decode_to_the_lab_ranges() {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 8 8] \
         /Resources << /XObject << /Im0 6 0 R >> >> /Contents 5 0 R >>",
    );
    b.stream(5, "", b"8 0 0 8 0 0 cm /Im0 Do");
    b.stream(
        6,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 \
         /ColorSpace [/Lab << /WhitePoint [0.9505 1 1.089] >>]",
        &[255, 128, 128],
    );
    let pix = render_clean(&b.build(1));
    let middle = ((pix.height / 2) * pix.width + pix.width / 2) as usize * 4;
    let pixel = &pix.data[middle..middle + 3];
    assert!(
        pixel.iter().all(|&v| v > 250),
        "expected near-white from L* = 100, got {pixel:?}"
    );
}
