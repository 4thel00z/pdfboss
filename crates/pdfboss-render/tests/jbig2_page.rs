//! End-to-end coverage of a symbol-coded JBIG2 image, from PDF bytes to
//! rasterized pixels.
//!
//! The unit tests inside `pdfboss-core` check the codec against fixtures its
//! own test encoder produced, which pins the decoder to the bit order of the
//! standard but cannot notice a break anywhere else on the path. This test
//! walks the whole path instead: a PDF file in memory, opened through
//! `Document`, its page rasterized through `render_page_reporting`, and the
//! pixels judged.
//!
//! It also judges them the way a reader would. A test that only asserts the
//! render returned `Ok` passes just as happily on a blank sheet, on a sheet of
//! noise, and on a sheet with the polarity inverted — three failures that look
//! nothing alike to a human and identical to an assertion on the error type. So
//! the assertions here are about ink: how much of it there is, and which bands
//! of the page hold it.
//!
//! The coded bytes are a fixed fixture rather than something regenerated per
//! run. That is the point of them: every other test in the tree feeds the
//! decoder bytes this codebase's own encoder just produced, so an encoder and a
//! decoder that drifted together would still agree. These bytes cannot drift.

use pdfboss_core::Document;
use pdfboss_render::{render_page_reporting, RenderOptions};

/// Page width of the fixture image, in pixels.
const WIDTH: u32 = 32;
/// Page height of the fixture image, in pixels.
const HEIGHT: u32 = 24;

/// The two symbols the fixture's dictionary carries, as rows of `'1'` and
/// `'0'`, and the three placements its text region makes.
///
/// Stating them here rather than only in the expected pixel counts is what
/// makes the fixture auditable: everything the assertions expect is derivable
/// from these shapes and coordinates.
const SYMBOL_A: [&str; 4] = ["101", "010", "101", "010"];
const SYMBOL_B: [&str; 4] = ["11111", "10001", "10001", "11111"];
/// `(symbol, x, y)` for each instance the text region places.
const PLACEMENTS: [(&[&str], u32, u32); 3] =
    [(&SYMBOL_A, 1, 2), (&SYMBOL_B, 5, 2), (&SYMBOL_A, 0, 10)];

/// A big-endian `u32` field.
fn u32be(value: u32) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

/// A short-form segment header (ITU-T T.88 7.2): segment number, the flags
/// byte whose low six bits are the type, the referred-to list, the page
/// association and the data length.
///
/// The page association is one byte and each referred-to number is one byte,
/// both of which the short forms of 7.2.3 and 7.2.5 permit while the segment
/// numbers stay small — which they do here, there being four segments.
fn segment_header(number: u32, kind: u8, refs: &[u8], data_len: u32) -> Vec<u8> {
    let mut out = u32be(number);
    out.push(kind);
    out.push((refs.len() as u8) << 5);
    out.extend_from_slice(refs);
    out.push(1); // page association
    out.extend_from_slice(&u32be(data_len));
    out
}

/// A segment header followed by its data.
fn segment(number: u32, kind: u8, refs: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = segment_header(number, kind, refs, data.len() as u32);
    out.extend_from_slice(data);
    out
}

/// The four AT pixel pairs of generic region template 0 at their nominal
/// offsets — (+3, −1), (−3, −1), (+2, −2), (−2, −2) — as the eight signed
/// bytes a symbol dictionary segment carries them in (T.88 6.2.5.3, 7.4.3.1.2).
fn nominal_at() -> Vec<u8> {
    let pairs: [(i8, i8); 4] = [(3, -1), (-3, -1), (2, -2), (-2, -2)];
    let mut out = Vec::new();
    for (dx, dy) in pairs {
        out.push(dx as u8);
        out.push(dy as u8);
    }
    out
}

/// The embedded-stream form of a scanned text page (T.88 Annex D.3): page
/// information, a symbol dictionary, a text region that refers to it, and end
/// of page.
///
/// This is the exact segment shape a symbol-coded scan uses — the two coding
/// modes that matter are the arithmetic symbol dictionary of 6.5 and the
/// arithmetic text region of 6.4, with no generic region segment anywhere.
///
/// The two arithmetic payloads are opaque by nature: each is a single MQ-coded
/// stream braiding the Annex A integer procedures together with, in the
/// dictionary's case, the pixel decisions of the symbols themselves. Every
/// field around them is spelled out.
fn jbig2_stream() -> Vec<u8> {
    let mut page_info = u32be(WIDTH);
    page_info.extend_from_slice(&u32be(HEIGHT));
    page_info.extend_from_slice(&u32be(0)); // x resolution, unstated
    page_info.extend_from_slice(&u32be(0)); // y resolution, unstated
    page_info.push(0); // default pixel 0, default combination operator OR
    page_info.extend_from_slice(&[0, 0]); // not striped

    // 7.4.3: flags, the AT pixels, the exported and new symbol counts, then
    // the coded data. Both counts are 2: the dictionary exports everything it
    // codes. The flags are all clear, which is arithmetic coding (SDHUFF 0),
    // no refinement or aggregation (SDREFAGG 0), and template 0.
    let mut dictionary = vec![0, 0];
    dictionary.extend_from_slice(&nominal_at());
    dictionary.extend_from_slice(&u32be(2)); // SDNUMEXSYMS
    dictionary.extend_from_slice(&u32be(2)); // SDNUMNEWSYMS
    dictionary.extend_from_slice(&[85, 99, 84, 85, 230, 236, 70, 191, 255, 172]);

    // 7.4.1 region segment information, then 7.4.4 text region flags and the
    // instance count. The region covers the page and composes with OR. The
    // flags are 0x0010: arithmetic (SBHUFF 0), no refinement, one row per
    // strip, TOPLEFT reference corner, untransposed, OR, a clear background
    // and no offset on the gaps between instances.
    let mut region = u32be(WIDTH);
    region.extend_from_slice(&u32be(HEIGHT));
    region.extend_from_slice(&u32be(0)); // region X on the page
    region.extend_from_slice(&u32be(0)); // region Y on the page
    region.push(0); // external combination operator: OR
    region.extend_from_slice(&[0, 16]); // text region flags
    region.extend_from_slice(&u32be(PLACEMENTS.len() as u32)); // SBNUMINSTANCES
    region.extend_from_slice(&[162, 195, 1, 202, 191, 255, 172]);

    let mut out = segment(0, 48, &[], &page_info); // page information
    out.extend_from_slice(&segment(1, 0, &[], &dictionary)); // symbol dictionary
    out.extend_from_slice(&segment(2, 6, &[1], &region)); // immediate text region
    out.extend_from_slice(&segment(3, 49, &[], &[])); // end of page
    out
}

/// A one-page PDF whose only content is the fixture image, drawn to fill the
/// page at one point per pixel.
///
/// `/Decode [0 1]` is the identity map, so the samples the filter returns are
/// painted as they stand: it is the filter that owes the image layer a 0 for
/// every inked pixel (ISO 32000-1 7.4.7 and 8.9.5.2). Stating it explicitly
/// rather than relying on the default is what makes this fixture a check on
/// the filter's polarity rather than on the default's.
fn pdf_bytes() -> Vec<u8> {
    let mut builder = pdfboss_testkit::PdfBuilder::new();
    builder.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    builder.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    builder.object(
        3,
        &format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {WIDTH} {HEIGHT}] \
             /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>"
        ),
    );
    builder.stream(
        4,
        "",
        format!("q {WIDTH} 0 0 {HEIGHT} 0 0 cm /Im0 Do Q").as_bytes(),
    );
    builder.stream(
        5,
        &format!(
            "/Type /XObject /Subtype /Image /Width {WIDTH} /Height {HEIGHT} \
             /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [0 1] \
             /Filter /JBIG2Decode"
        ),
        &jbig2_stream(),
    );
    builder.build(1)
}

/// The pixels the fixture's placements ink, as a `HEIGHT`-long count per row.
fn expected_ink_per_row() -> Vec<u32> {
    let mut rows = vec![0u32; HEIGHT as usize];
    for (symbol, _, y) in PLACEMENTS {
        for (dy, row) in symbol.iter().enumerate() {
            rows[y as usize + dy] += row.bytes().filter(|&b| b == b'1').count() as u32;
        }
    }
    rows
}

/// Renders the fixture and returns, per device row, how many of its pixels are
/// dark — the same measurement one would make by eye on a scanned page, and
/// the one that separates a decoded page from a blank, noisy or inverted one.
fn dark_pixels_per_row(scale: f32) -> (u32, u32, Vec<u32>) {
    let doc = Document::load(pdf_bytes()).expect("the fixture PDF opens");
    let page = doc.page(0).expect("the fixture has one page");
    let (pixmap, report) = render_page_reporting(&doc, &page, scale, &RenderOptions::default())
        .expect("the page renders");

    // A JBIG2 image the rasterizer could not decode is skipped, not fatal: the
    // page would still render, still be blank, and still return Ok. The report
    // is the only place that shows the difference.
    assert!(
        report.is_empty(),
        "content was skipped: {:?}",
        report.warnings()
    );

    let rows = (0..pixmap.height)
        .map(|y| {
            (0..pixmap.width)
                .filter(|x| {
                    let i = ((y * pixmap.width + x) * 4) as usize;
                    let (r, g, b) = (
                        u32::from(pixmap.data[i]),
                        u32::from(pixmap.data[i + 1]),
                        u32::from(pixmap.data[i + 2]),
                    );
                    (r * 299 + g * 587 + b * 114) / 1000 < 128
                })
                .count() as u32
        })
        .collect();
    (pixmap.width, pixmap.height, rows)
}

/// The page is neither blank nor black nor noise.
///
/// A page of body text is a few per cent ink. Nothing painted leaves 0, a
/// dropped inversion leaves nearly 100, and an arithmetic decoder that has lost
/// synchronisation leaves roughly 50 — the three ways this could fail while
/// still returning `Ok`, and all three are outside the band.
#[test]
fn a_symbol_coded_page_renders_as_text_rather_than_blank_or_black() {
    let (width, height, rows) = dark_pixels_per_row(1.0);
    assert_eq!((width, height), (WIDTH, HEIGHT));

    let dark: u32 = rows.iter().sum();
    let total = width * height;
    assert!(
        dark > 0,
        "nothing was painted: the image decoded to an empty page"
    );
    assert!(
        dark * 100 < total * 20,
        "{dark} of {total} pixels are dark, far past what text covers: \
         the samples are probably inverted"
    );
}

/// The ink lands in the rows the placements put it in.
///
/// Three instances across two strips leave a distinctive profile: two blank
/// rows, four inked, four blank, four inked, then ten blank. Both the amount of
/// ink and its distribution have to match, so a page that happens to average
/// the right coverage while painting the wrong thing still fails — and because
/// the two blank gaps differ in size, so does a page rendered upside down.
#[test]
fn the_instances_land_in_the_rows_the_text_region_places_them_in() {
    let (_, height, rows) = dark_pixels_per_row(1.0);
    let expected = expected_ink_per_row();
    assert_eq!(height as usize, expected.len());
    for (y, (&got, &want)) in rows.iter().zip(&expected).enumerate() {
        assert_eq!(
            got > 0,
            want > 0,
            "row {y}: {got} dark pixels, expected {}",
            if want > 0 { "some" } else { "none" },
        );
    }
    assert_eq!(
        rows.iter().sum::<u32>(),
        expected.iter().sum::<u32>(),
        "row profile {rows:?} against {expected:?}",
    );
}

/// The same page at eight pixels per sample, which is the scale a reader
/// actually views a scan at.
///
/// Upsampling is where a nearest-neighbour and an interpolating sampler part
/// company, and where an off-by-one in the image transform stops being
/// invisible. The ink fraction has to survive it: the same marks, eight times
/// the area.
#[test]
fn the_ink_fraction_survives_upsampling() {
    let (width, height, rows) = dark_pixels_per_row(8.0);
    assert_eq!((width, height), (WIDTH * 8, HEIGHT * 8));

    let dark = f64::from(rows.iter().sum::<u32>());
    let fraction = dark / f64::from(width * height);
    let expected =
        f64::from(expected_ink_per_row().iter().sum::<u32>()) / f64::from(WIDTH * HEIGHT);
    assert!(
        (fraction - expected).abs() < 0.02,
        "ink is {fraction:.3} at 8x against {expected:.3} at 1x",
    );
}
