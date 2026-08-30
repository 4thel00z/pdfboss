//! Output formats beyond PNG: PPM and BMP are a header plus one packing
//! pass over the pixmap, so every test here parses the header back and
//! compares each pixel with the pixmap it came from.

use pdfboss_render::{ImageFormat, Pixmap, PngCompression};

/// A two-axis gradient with an opaque square stamped on it, so rows and
/// columns are distinguishable and a transposed or flipped encoder fails.
fn gradient_pixmap(width: u32, height: u32) -> Pixmap {
    let mut pix = Pixmap::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            pix.data[i] = (x * 7) as u8;
            pix.data[i + 1] = (y * 11) as u8;
            pix.data[i + 2] = (x + y) as u8;
            pix.data[i + 3] = 255;
        }
    }
    pix
}

fn u16le(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32le(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn i32le(bytes: &[u8], at: usize) -> i32 {
    u32le(bytes, at) as i32
}

#[test]
fn ppm_is_p6_with_the_pixmap_dimensions_and_rgb_rows() {
    let pix = gradient_pixmap(5, 3);
    let ppm = pix.encode(ImageFormat::Ppm).unwrap();
    let header = b"P6\n5 3\n255\n";
    assert!(
        ppm.starts_with(header),
        "header: {:?}",
        &ppm[..header.len()]
    );
    let pixels = &ppm[header.len()..];
    assert_eq!(pixels.len(), 5 * 3 * 3);
    for (rgb, rgba) in pixels
        .as_chunks::<3>()
        .0
        .iter()
        .zip(pix.data.as_chunks::<4>().0)
    {
        assert_eq!(&rgb[..], &rgba[..3]);
    }
}

#[test]
fn bmp_header_describes_a_24_bit_bottom_up_image() {
    let pix = gradient_pixmap(5, 3);
    let bmp = pix.encode(ImageFormat::Bmp).unwrap();
    let stride = 16; // 5 * 3 = 15 bytes, padded to the next multiple of 4
    assert_eq!(&bmp[..2], b"BM");
    assert_eq!(u32le(&bmp, 2) as usize, bmp.len(), "file size field");
    assert_eq!(u32le(&bmp, 10), 54, "pixel data offset");
    assert_eq!(u32le(&bmp, 14), 40, "BITMAPINFOHEADER size");
    assert_eq!(i32le(&bmp, 18), 5, "width");
    assert_eq!(i32le(&bmp, 22), 3, "positive height means bottom-up rows");
    assert_eq!(u16le(&bmp, 26), 1, "planes");
    assert_eq!(u16le(&bmp, 28), 24, "bits per pixel");
    assert_eq!(u32le(&bmp, 30), 0, "BI_RGB, uncompressed");
    assert_eq!(u32le(&bmp, 34) as usize, stride * 3, "image size");
    assert_eq!(bmp.len(), 54 + stride * 3);
}

#[test]
fn bmp_rows_are_bgr_bottom_up_and_padded_to_four_bytes() {
    let pix = gradient_pixmap(3, 2);
    let bmp = pix.encode(ImageFormat::Bmp).unwrap();
    let stride = 12; // 3 * 3 = 9 bytes of pixels, 3 bytes of padding
    for y in 0..2usize {
        for x in 0..3usize {
            let src = (y * 3 + x) * 4;
            let dst = 54 + (1 - y) * stride + x * 3;
            assert_eq!(
                &bmp[dst..dst + 3],
                &[pix.data[src + 2], pix.data[src + 1], pix.data[src]],
                "pixel ({x}, {y})"
            );
        }
        let pad = 54 + (1 - y) * stride + 9;
        assert_eq!(&bmp[pad..pad + 3], &[0, 0, 0], "row {y} padding");
    }
}

#[test]
fn bmp_size_matches_the_padded_stride_for_widths_one_to_four() {
    for width in 1..=4u32 {
        let bmp = gradient_pixmap(width, 2).encode(ImageFormat::Bmp).unwrap();
        let stride = ((width as usize * 3) + 3) & !3;
        assert_eq!(bmp.len(), 54 + stride * 2, "width {width}");
    }
}

#[test]
fn png_through_encode_is_byte_identical_to_encode_png_with() {
    let pix = gradient_pixmap(64, 48);
    for level in [
        PngCompression::None,
        PngCompression::Fast,
        PngCompression::Balanced,
        PngCompression::Best,
    ] {
        assert_eq!(
            pix.encode(ImageFormat::Png(level)).unwrap(),
            pix.encode_png_with(level).unwrap(),
            "{level:?}"
        );
    }
}

#[test]
fn from_name_accepts_the_three_formats_case_insensitively_and_rejects_others() {
    assert_eq!(
        ImageFormat::from_name("png"),
        Some(ImageFormat::Png(PngCompression::default()))
    );
    assert_eq!(ImageFormat::from_name("PNG"), ImageFormat::from_name("png"));
    assert_eq!(ImageFormat::from_name("ppm"), Some(ImageFormat::Ppm));
    assert_eq!(ImageFormat::from_name("bmp"), Some(ImageFormat::Bmp));
    assert_eq!(ImageFormat::from_name("tiff"), None);
    assert_eq!(ImageFormat::from_name(""), None);
}
