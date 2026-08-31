//! JPEG output: baseline sequential, 4:4:4, quality-scaled tables. Every
//! test decodes what the encoder wrote and holds it against the source
//! pixels, so the encoder is judged by what a decoder reconstructs.

use pdfboss_render::{ImageFormat, Pixmap};

/// A smooth two-axis gradient with a hard-edged square stamped on it: the
/// gradient exercises DC prediction and low frequencies, the square's edges
/// the high-frequency AC path and run-length coding.
fn photo_pixmap(width: u32, height: u32) -> Pixmap {
    let mut pix = Pixmap::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let inside = x > width / 4 && x < width * 3 / 4 && y > height / 4 && y < height * 3 / 4;
            let (r, g, b) = if inside {
                (20, 60, 200)
            } else {
                (
                    (x * 255 / width.max(1)) as u8,
                    (y * 255 / height.max(1)) as u8,
                    ((x + y) * 255 / (width + height).max(1)) as u8,
                )
            };
            pix.data[i..i + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    pix
}

fn rgb(pix: &Pixmap) -> Vec<u8> {
    pix.data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect()
}

fn decode(jpeg: &[u8]) -> (u16, u16, Vec<u8>) {
    let mut decoder = jpeg_decoder::Decoder::new(jpeg);
    let pixels = decoder.decode().expect("decoder rejected the stream");
    let info = decoder.info().unwrap();
    assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
    (info.width, info.height, pixels)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mse = a
        .iter()
        .zip(b)
        .map(|(&p, &q)| {
            let d = p as f64 - q as f64;
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10()
}

#[test]
fn jpeg_round_trips_through_a_decoder_above_35_db() {
    let pix = photo_pixmap(64, 48);
    let jpeg = pix.encode(ImageFormat::Jpeg { quality: 90 }).unwrap();
    let (width, height, pixels) = decode(&jpeg);
    assert_eq!((width, height), (64, 48));
    let db = psnr(&pixels, &rgb(&pix));
    assert!(db >= 35.0, "PSNR {db:.1} dB");
}

#[test]
fn jpeg_handles_edges_that_are_not_block_aligned() {
    let pix = photo_pixmap(13, 11);
    let jpeg = pix.encode(ImageFormat::Jpeg { quality: 90 }).unwrap();
    let (width, height, pixels) = decode(&jpeg);
    assert_eq!((width, height), (13, 11));
    let db = psnr(&pixels, &rgb(&pix));
    assert!(db >= 30.0, "PSNR {db:.1} dB");
}

#[test]
fn jpeg_quality_orders_file_size_and_fidelity() {
    let pix = photo_pixmap(96, 80);
    let at = |quality| pix.encode(ImageFormat::Jpeg { quality }).unwrap();
    let (low, mid, high) = (at(25), at(60), at(95));
    assert!(
        low.len() < mid.len() && mid.len() < high.len(),
        "{} {} {}",
        low.len(),
        mid.len(),
        high.len()
    );
    let source = rgb(&pix);
    let db = |jpeg: &[u8]| psnr(&decode(jpeg).2, &source);
    assert!(db(&low) < db(&high), "{:.1} vs {:.1}", db(&low), db(&high));
}

/// The segments between SOI and the scan, as `(marker, payload)` pairs.
fn segments(jpeg: &[u8]) -> Vec<(u8, &[u8])> {
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "SOI");
    let mut out = Vec::new();
    let mut at = 2;
    loop {
        assert_eq!(jpeg[at], 0xFF, "marker prefix at {at}");
        let marker = jpeg[at + 1];
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        out.push((marker, &jpeg[at + 4..at + 2 + len]));
        at += 2 + len;
        if marker == 0xDA {
            return out;
        }
    }
}

#[test]
fn jpeg_markers_describe_a_baseline_444_jfif_image() {
    let jpeg = photo_pixmap(40, 24)
        .encode(ImageFormat::Jpeg { quality: 75 })
        .unwrap();
    assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "EOI");
    let segments = segments(&jpeg);
    let (first, app0) = segments[0];
    assert_eq!(first, 0xE0, "APP0 comes first");
    assert_eq!(&app0[..5], b"JFIF\0");
    let (_, sof) = *segments.iter().find(|(m, _)| *m == 0xC0).expect("SOF0");
    assert_eq!(sof[0], 8, "sample precision");
    assert_eq!(u16::from_be_bytes([sof[1], sof[2]]), 24, "height");
    assert_eq!(u16::from_be_bytes([sof[3], sof[4]]), 40, "width");
    assert_eq!(sof[5], 3, "components");
    for c in 0..3 {
        assert_eq!(sof[6 + c * 3 + 1], 0x11, "component {c} sampling 1x1");
    }
    assert_eq!(
        segments.iter().filter(|(m, _)| *m == 0xDB).count(),
        2,
        "two DQT segments"
    );
    assert_eq!(
        segments.iter().filter(|(m, _)| *m == 0xC4).count(),
        4,
        "four DHT segments"
    );
}

#[test]
fn from_name_accepts_jpeg_and_jpg_at_the_default_quality() {
    assert_eq!(
        ImageFormat::from_name("jpeg"),
        Some(ImageFormat::Jpeg { quality: 90 })
    );
    assert_eq!(
        ImageFormat::from_name("jpg"),
        ImageFormat::from_name("jpeg")
    );
    assert_eq!(
        ImageFormat::from_name("JPG"),
        ImageFormat::from_name("jpeg")
    );
}
