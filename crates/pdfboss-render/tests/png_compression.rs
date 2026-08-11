//! PNG encode compression levels: every level must round-trip the exact
//! pixels — compression only trades encode time against file size, never
//! image content — and the default-compression path must stay byte-identical
//! to what `encode_png` always produced, so callers that never asked for a
//! level see no change.

use pdfboss_render::{Pixmap, PngCompression};

/// A pixmap with enough structure to compress well: a two-axis gradient
/// with an opaque square stamped on it. Compressible content is what makes
/// the size ordering between `None` and `Best` observable.
fn gradient_pixmap() -> Pixmap {
    let mut pix = Pixmap::new(128, 96);
    for y in 0..96u32 {
        for x in 0..128u32 {
            let i = ((y * 128 + x) * 4) as usize;
            pix.data[i] = (x * 2) as u8;
            pix.data[i + 1] = (y * 2) as u8;
            pix.data[i + 2] = 128;
            pix.data[i + 3] = 255;
        }
    }
    for y in 20..60u32 {
        for x in 30..90u32 {
            let i = ((y * 128 + x) * 4) as usize;
            pix.data[i..i + 4].copy_from_slice(&[10, 200, 40, 255]);
        }
    }
    pix
}

fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size().expect("output size")];
    let info = reader.next_frame(&mut buf).expect("png frame");
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

#[test]
fn every_level_round_trips_the_exact_pixels() {
    let pix = gradient_pixmap();
    for level in [
        PngCompression::None,
        PngCompression::Fast,
        PngCompression::Balanced,
        PngCompression::Best,
    ] {
        let png = pix.encode_png_with(level).expect("encode");
        let (w, h, data) = decode(&png);
        assert_eq!((w, h), (pix.width, pix.height), "{level:?} dimensions");
        assert_eq!(data, pix.data, "{level:?} pixels");
    }
}

#[test]
fn uncompressed_output_is_larger_than_best() {
    let pix = gradient_pixmap();
    let none = pix.encode_png_with(PngCompression::None).expect("encode");
    let best = pix.encode_png_with(PngCompression::Best).expect("encode");
    assert!(
        none.len() > best.len(),
        "expected None ({}) > Best ({})",
        none.len(),
        best.len()
    );
}

#[test]
fn encode_png_is_byte_identical_to_the_default_level() {
    let pix = gradient_pixmap();
    let plain = pix.encode_png().expect("encode");
    let with_default = pix
        .encode_png_with(PngCompression::default())
        .expect("encode");
    assert_eq!(plain, with_default);
}
