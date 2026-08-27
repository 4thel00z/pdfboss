//! Image import for document creation. JPEG passes through untouched as
//! `/DCTDecode` (dimensions sniffed from its SOF marker); PNG is decoded
//! to a raster with its alpha split into an `/SMask`; raw rasters cover
//! generated content. No encode-side JPX/JBIG2/CCITT — by design.

use pdfboss_core::{Dict, Name, ObjRef, Object};

use crate::error::{Error, Result};
use crate::writer::Writer;

/// A decoded or passthrough image ready to embed as an image XObject.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageData {
    width: u32,
    height: u32,
    kind: ImageKind,
}

/// How the pixels are held.
#[derive(Debug, Clone, PartialEq)]
enum ImageKind {
    /// Original JPEG bytes, embedded as-is with `/Filter /DCTDecode`.
    Jpeg { data: Vec<u8>, gray: bool },
    /// Uncompressed raster, Flate-compressed on embedding.
    Raster {
        data: Vec<u8>,
        color: RasterColor,
        smask: Option<Vec<u8>>,
    },
}

/// Raster sample layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RasterColor {
    /// 8-bit DeviceGray.
    Gray8,
    /// 8-bit DeviceRGB, samples interleaved.
    Rgb8,
    /// 1-bit DeviceGray, rows packed MSB-first and byte-padded.
    Mono1,
}

impl ImageData {
    /// Imports a PNG. Truecolor and grayscale (8- and 16-bit, the latter
    /// reduced to 8), palette (expanded), and alpha (split to `/SMask`)
    /// are all supported.
    pub fn png(bytes: &[u8]) -> Result<ImageData> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder
            .read_info()
            .map_err(|e| Error::Image(format!("png header: {e}")))?;
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| Error::Image("png output buffer size overflows".into()))?;
        let mut buf = vec![0u8; size];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| Error::Image(format!("png pixel data: {e}")))?;
        buf.truncate(info.buffer_size());
        if info.bit_depth != png::BitDepth::Eight {
            return Err(Error::Image(format!(
                "png bit depth {:?} survived expansion",
                info.bit_depth
            )));
        }
        let kind = match info.color_type {
            png::ColorType::Grayscale => ImageKind::Raster {
                data: buf,
                color: RasterColor::Gray8,
                smask: None,
            },
            png::ColorType::GrayscaleAlpha => {
                let (gray, alpha) = split_alpha(&buf, 1);
                ImageKind::Raster {
                    data: gray,
                    color: RasterColor::Gray8,
                    smask: Some(alpha),
                }
            }
            png::ColorType::Rgb => ImageKind::Raster {
                data: buf,
                color: RasterColor::Rgb8,
                smask: None,
            },
            png::ColorType::Rgba => {
                let (rgb, alpha) = split_alpha(&buf, 3);
                ImageKind::Raster {
                    data: rgb,
                    color: RasterColor::Rgb8,
                    smask: Some(alpha),
                }
            }
            other => {
                return Err(Error::Image(format!(
                    "png color type {other:?} survived expansion"
                )));
            }
        };
        Ok(ImageData {
            width: info.width,
            height: info.height,
            kind,
        })
    }

    /// Imports a baseline or progressive JPEG by passthrough. Dimensions
    /// and component count are read from the SOF marker; grayscale and
    /// three-component (YCbCr/RGB) images are supported, anything else is
    /// an error.
    pub fn jpeg(bytes: &[u8]) -> Result<ImageData> {
        if bytes.len() < 2 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
            return Err(Error::Image("jpeg missing SOI marker".into()));
        }
        let mut pos = 2usize;
        loop {
            if pos >= bytes.len() {
                return Err(Error::Image("jpeg truncated before a SOF marker".into()));
            }
            if bytes[pos] != 0xFF {
                return Err(Error::Image(format!(
                    "jpeg expected a marker at byte {pos}, found 0x{:02X}",
                    bytes[pos]
                )));
            }
            while pos < bytes.len() && bytes[pos] == 0xFF {
                pos += 1;
            }
            if pos >= bytes.len() {
                return Err(Error::Image("jpeg truncated inside a marker".into()));
            }
            let marker = bytes[pos];
            pos += 1;
            match marker {
                0xC0..=0xC2 => return sniff_sof(bytes, pos),
                0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                    return Err(Error::Image(format!(
                        "jpeg SOF{} (marker 0xFF{marker:02X}) is not supported for passthrough",
                        marker as usize - 0xC0
                    )));
                }
                0xD9 => return Err(Error::Image("jpeg ended (EOI) before a SOF marker".into())),
                0xDA => {
                    return Err(Error::Image("jpeg scan started before a SOF marker".into()));
                }
                0x00 => return Err(Error::Image("jpeg stray 0xFF00 outside a scan".into())),
                0x01 | 0xD0..=0xD7 => {}
                other => pos = skip_segment(bytes, pos, other)?,
            }
        }
    }

    /// Wraps an 8-bit grayscale raster; `data` is `width * height` bytes.
    pub fn gray8(width: u32, height: u32, data: Vec<u8>) -> Result<ImageData> {
        let expected = checked_dims("gray8", width, height)?;
        check_len("gray8", expected, data.len())?;
        Ok(ImageData {
            width,
            height,
            kind: ImageKind::Raster {
                data,
                color: RasterColor::Gray8,
                smask: None,
            },
        })
    }

    /// Wraps an 8-bit RGB raster; `data` is `width * height * 3` bytes.
    pub fn rgb8(width: u32, height: u32, data: Vec<u8>) -> Result<ImageData> {
        let expected = checked_dims("rgb8", width, height)? * 3;
        check_len("rgb8", expected, data.len())?;
        Ok(ImageData {
            width,
            height,
            kind: ImageKind::Raster {
                data,
                color: RasterColor::Rgb8,
                smask: None,
            },
        })
    }

    /// Wraps a 1-bit raster; rows are packed MSB-first, each row padded to
    /// a whole byte. A set bit is black (sample 0).
    pub fn mono(width: u32, height: u32, data: Vec<u8>) -> Result<ImageData> {
        checked_dims("mono", width, height)?;
        let expected = (width as usize).div_ceil(8) * height as usize;
        check_len("mono", expected, data.len())?;
        Ok(ImageData {
            width,
            height,
            kind: ImageKind::Raster {
                data,
                color: RasterColor::Mono1,
                smask: None,
            },
        })
    }

    /// Pixel width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Pixel height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Emits this image (and its soft mask, if any) into `w` as image
    /// XObject(s), returning the image object's reference. The soft mask
    /// is put first, so its object number precedes the image's.
    pub(crate) fn build_xobject(&self, w: &mut Writer) -> ObjRef {
        match &self.kind {
            ImageKind::Jpeg { data, gray } => {
                let mut dict = self.base_dict();
                dict.insert(name("Filter"), Object::Name(name("DCTDecode")));
                dict.insert(name("BitsPerComponent"), Object::Int(8));
                dict.insert(name("ColorSpace"), Object::Name(gray_or_rgb(*gray)));
                w.put_stream_raw(dict, data.clone())
            }
            ImageKind::Raster { data, color, smask } => {
                let mask_ref = smask.as_ref().map(|alpha| {
                    let mut mask = self.base_dict();
                    mask.insert(name("BitsPerComponent"), Object::Int(8));
                    mask.insert(name("ColorSpace"), Object::Name(name("DeviceGray")));
                    w.put_stream(mask, alpha.clone())
                });
                let mut dict = self.base_dict();
                match color {
                    RasterColor::Gray8 => {
                        dict.insert(name("BitsPerComponent"), Object::Int(8));
                        dict.insert(name("ColorSpace"), Object::Name(name("DeviceGray")));
                    }
                    RasterColor::Rgb8 => {
                        dict.insert(name("BitsPerComponent"), Object::Int(8));
                        dict.insert(name("ColorSpace"), Object::Name(name("DeviceRGB")));
                    }
                    RasterColor::Mono1 => {
                        dict.insert(name("BitsPerComponent"), Object::Int(1));
                        dict.insert(name("ColorSpace"), Object::Name(name("DeviceGray")));
                        dict.insert(
                            name("Decode"),
                            Object::Array(vec![Object::Int(1), Object::Int(0)]),
                        );
                    }
                }
                if let Some(mask_ref) = mask_ref {
                    dict.insert(name("SMask"), Object::Ref(mask_ref));
                }
                w.put_stream(dict, data.clone())
            }
        }
    }

    /// The dictionary entries every image XObject shares.
    fn base_dict(&self) -> Dict {
        let mut dict = Dict::new();
        dict.insert(name("Type"), Object::Name(name("XObject")));
        dict.insert(name("Subtype"), Object::Name(name("Image")));
        dict.insert(name("Width"), Object::Int(i64::from(self.width)));
        dict.insert(name("Height"), Object::Int(i64::from(self.height)));
        dict
    }
}

/// A `Name` from a string literal.
fn name(text: &str) -> Name {
    Name(text.to_string())
}

/// The device color space name for a one- or three-component image.
fn gray_or_rgb(gray: bool) -> Name {
    if gray {
        name("DeviceGray")
    } else {
        name("DeviceRGB")
    }
}

fn split_alpha(samples: &[u8], color_channels: usize) -> (Vec<u8>, Vec<u8>) {
    let pixels = samples.len() / (color_channels + 1);
    let mut color: Vec<u8> = Vec::with_capacity(pixels * color_channels);
    let mut alpha: Vec<u8> = Vec::with_capacity(pixels);
    for px in samples.chunks_exact(color_channels + 1) {
        color.extend_from_slice(&px[..color_channels]);
        alpha.push(px[color_channels]);
    }
    (color, alpha)
}

fn sniff_sof(bytes: &[u8], pos: usize) -> Result<ImageData> {
    if pos + 8 > bytes.len() {
        return Err(Error::Image("jpeg truncated inside its SOF marker".into()));
    }
    let precision = bytes[pos + 2];
    if precision != 8 {
        return Err(Error::Image(format!(
            "jpeg sample precision is {precision}, only 8 is supported"
        )));
    }
    let height = u32::from(u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]));
    let width = u32::from(u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]));
    if width == 0 || height == 0 {
        return Err(Error::Image(format!(
            "jpeg declares degenerate dimensions {width}x{height}"
        )));
    }
    let gray = match bytes[pos + 7] {
        1 => true,
        3 => false,
        n => {
            return Err(Error::Image(format!(
                "jpeg has {n} components, only 1 or 3 are supported"
            )));
        }
    };
    Ok(ImageData {
        width,
        height,
        kind: ImageKind::Jpeg {
            data: bytes.to_vec(),
            gray,
        },
    })
}

fn skip_segment(bytes: &[u8], pos: usize, marker: u8) -> Result<usize> {
    if pos + 2 > bytes.len() {
        return Err(Error::Image(format!(
            "jpeg truncated in the length of marker 0xFF{marker:02X}"
        )));
    }
    let len = usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]));
    if len < 2 {
        return Err(Error::Image(format!(
            "jpeg marker 0xFF{marker:02X} has segment length {len}, minimum is 2"
        )));
    }
    if pos + len > bytes.len() {
        return Err(Error::Image(format!(
            "jpeg truncated inside the segment of marker 0xFF{marker:02X}"
        )));
    }
    Ok(pos + len)
}

fn checked_dims(label: &str, width: u32, height: u32) -> Result<usize> {
    if width == 0 || height == 0 {
        return Err(Error::Image(format!(
            "{label} raster: dimensions {width}x{height} must be nonzero"
        )));
    }
    Ok(width as usize * height as usize)
}

fn check_len(label: &str, expected: usize, got: usize) -> Result<()> {
    if got != expected {
        return Err(Error::Image(format!(
            "{label} raster: expected {expected} bytes, got {got}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ImageData, ImageKind, RasterColor};
    use crate::error::Error;
    use std::io::Write;

    fn encode_png(
        width: u32,
        height: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        palette: Option<&[u8]>,
        data: &[u8],
    ) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(color);
        enc.set_depth(depth);
        if let Some(p) = palette {
            enc.set_palette(p.to_vec());
        }
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        out
    }

    fn png_chunk(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut chunk: Vec<u8> = Vec::new();
        chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        chunk.extend_from_slice(name);
        chunk.extend_from_slice(payload);
        let mut crc = flate2::Crc::new();
        crc.update(name);
        crc.update(payload);
        chunk.extend_from_slice(&crc.sum().to_be_bytes());
        chunk
    }

    fn interlaced_gray_2x2(pixels: [u8; 4]) -> Vec<u8> {
        let [p00, p10, p01, p11] = pixels;
        let mut ihdr: Vec<u8> = Vec::new();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 1]);
        let raw: [u8; 7] = [0, p00, 0, p10, 0, p01, p11];
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        zlib.write_all(&raw).unwrap();
        let idat = zlib.finish().unwrap();
        let mut file: Vec<u8> = vec![137, 80, 78, 71, 13, 10, 26, 10];
        file.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        file.extend_from_slice(&png_chunk(b"IDAT", &idat));
        file.extend_from_slice(&png_chunk(b"IEND", &[]));
        file
    }

    fn raster(img: &ImageData) -> (&[u8], RasterColor, Option<&[u8]>) {
        match &img.kind {
            ImageKind::Raster { data, color, smask } => (data, *color, smask.as_deref()),
            ImageKind::Jpeg { .. } => panic!("expected raster, got jpeg"),
        }
    }

    fn image_message(result: crate::error::Result<ImageData>) -> String {
        match result {
            Err(Error::Image(msg)) => msg,
            other => panic!("expected Error::Image, got {other:?}"),
        }
    }

    #[test]
    fn png_rgb() {
        let data: [u8; 12] = [255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 8, 7];
        let bytes = encode_png(2, 2, png::ColorType::Rgb, png::BitDepth::Eight, None, &data);
        let img = ImageData::png(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (2, 2));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Rgb8);
        assert_eq!(pixels, data);
        assert!(smask.is_none());
    }

    #[test]
    fn png_rgba_splits_smask() {
        let data: [u8; 16] = [1, 2, 3, 128, 4, 5, 6, 255, 7, 8, 9, 0, 10, 11, 12, 64];
        let bytes = encode_png(
            2,
            2,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            None,
            &data,
        );
        let img = ImageData::png(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (2, 2));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Rgb8);
        assert_eq!(pixels, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(smask, Some([128, 255, 0, 64].as_slice()));
    }

    #[test]
    fn png_gray() {
        let data: [u8; 6] = [0, 60, 120, 180, 220, 255];
        let bytes = encode_png(
            3,
            2,
            png::ColorType::Grayscale,
            png::BitDepth::Eight,
            None,
            &data,
        );
        let img = ImageData::png(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (3, 2));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Gray8);
        assert_eq!(pixels, data);
        assert!(smask.is_none());
    }

    #[test]
    fn png_gray_alpha_splits_smask() {
        let data: [u8; 4] = [50, 200, 100, 30];
        let bytes = encode_png(
            2,
            1,
            png::ColorType::GrayscaleAlpha,
            png::BitDepth::Eight,
            None,
            &data,
        );
        let img = ImageData::png(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Gray8);
        assert_eq!(pixels, [50, 100]);
        assert_eq!(smask, Some([200, 30].as_slice()));
    }

    #[test]
    fn png_palette_expands_to_rgb() {
        let palette: [u8; 6] = [255, 0, 0, 0, 255, 0];
        let bytes = encode_png(
            2,
            1,
            png::ColorType::Indexed,
            png::BitDepth::Eight,
            Some(&palette),
            &[0, 1],
        );
        let img = ImageData::png(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Rgb8);
        assert_eq!(pixels, [255, 0, 0, 0, 255, 0]);
        assert!(smask.is_none());
    }

    #[test]
    fn png_sixteen_bit_reduces_to_eight() {
        let data: [u8; 12] = [
            0xAB, 0xCD, 0x12, 0x34, 0xFF, 0xFF, 0x00, 0x01, 0x80, 0x00, 0x7F, 0xFE,
        ];
        let bytes = encode_png(
            2,
            1,
            png::ColorType::Rgb,
            png::BitDepth::Sixteen,
            None,
            &data,
        );
        let img = ImageData::png(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Rgb8);
        assert_eq!(pixels, [0xAB, 0x12, 0xFF, 0x00, 0x80, 0x7F]);
        assert!(smask.is_none());
    }

    #[test]
    fn png_interlaced_deinterlaces() {
        let bytes = interlaced_gray_2x2([10, 20, 30, 40]);
        let img = ImageData::png(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (2, 2));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Gray8);
        assert_eq!(pixels, [10, 20, 30, 40]);
        assert!(smask.is_none());
    }

    #[test]
    fn png_garbage_is_image_error() {
        let msg = image_message(ImageData::png(&[1, 2, 3, 4, 5, 6, 7, 8]));
        assert!(!msg.is_empty());
    }

    fn jpeg_sof(marker: u8, precision: u8, width: u16, height: u16, components: u8) -> Vec<u8> {
        let mut seg: Vec<u8> = vec![0xFF, marker];
        seg.extend_from_slice(&(8 + 3 * components as u16).to_be_bytes());
        seg.push(precision);
        seg.extend_from_slice(&height.to_be_bytes());
        seg.extend_from_slice(&width.to_be_bytes());
        seg.push(components);
        for id in 0..components {
            seg.extend_from_slice(&[id + 1, 0x11, 0]);
        }
        seg
    }

    fn jpeg_app1() -> Vec<u8> {
        let payload = b"pdfboss-write";
        let mut seg: Vec<u8> = vec![0xFF, 0xE1];
        seg.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        seg.extend_from_slice(payload);
        seg
    }

    fn jpeg_bytes(segments: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = vec![0xFF, 0xD8];
        for seg in segments {
            out.extend_from_slice(seg);
        }
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    #[test]
    fn jpeg_color_baseline() {
        let bytes = jpeg_bytes(&[jpeg_app1(), jpeg_sof(0xC0, 8, 5, 7, 3)]);
        let img = ImageData::jpeg(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (5, 7));
        match &img.kind {
            ImageKind::Jpeg { data, gray } => {
                assert_eq!(data, &bytes);
                assert!(!gray);
            }
            ImageKind::Raster { .. } => panic!("expected jpeg passthrough"),
        }
    }

    #[test]
    fn jpeg_gray_with_fill_bytes() {
        let mut sof = jpeg_sof(0xC1, 8, 9, 4, 1);
        sof.insert(0, 0xFF);
        let bytes = jpeg_bytes(&[jpeg_app1(), sof]);
        let img = ImageData::jpeg(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (9, 4));
        match &img.kind {
            ImageKind::Jpeg { data, gray } => {
                assert_eq!(data, &bytes);
                assert!(gray);
            }
            ImageKind::Raster { .. } => panic!("expected jpeg passthrough"),
        }
    }

    #[test]
    fn jpeg_progressive_sof2() {
        let bytes = jpeg_bytes(&[jpeg_sof(0xC2, 8, 640, 480, 3)]);
        let img = ImageData::jpeg(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (640, 480));
    }

    #[test]
    fn jpeg_four_components_rejected_naming_count() {
        let bytes = jpeg_bytes(&[jpeg_sof(0xC0, 8, 4, 4, 4)]);
        let msg = image_message(ImageData::jpeg(&bytes));
        assert!(msg.contains('4'), "message should name the count: {msg}");
    }

    #[test]
    fn jpeg_lossless_sof3_rejected() {
        let bytes = jpeg_bytes(&[jpeg_sof(0xC3, 8, 4, 4, 1)]);
        image_message(ImageData::jpeg(&bytes));
    }

    #[test]
    fn jpeg_truncated_rejected() {
        let full = jpeg_bytes(&[jpeg_app1(), jpeg_sof(0xC0, 8, 5, 7, 3)]);
        image_message(ImageData::jpeg(&full[..6]));
    }

    #[test]
    fn jpeg_missing_soi_rejected() {
        image_message(ImageData::jpeg(&[0x00, 0x11, 0x22]));
    }

    #[test]
    fn jpeg_non_eight_bit_precision_rejected() {
        let bytes = jpeg_bytes(&[jpeg_sof(0xC1, 12, 4, 4, 1)]);
        let msg = image_message(ImageData::jpeg(&bytes));
        assert!(
            msg.contains("12"),
            "message should name the precision: {msg}"
        );
    }

    #[test]
    fn gray8_accepts_exact_length() {
        let img = ImageData::gray8(2, 3, vec![1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!((img.width(), img.height()), (2, 3));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Gray8);
        assert_eq!(pixels, [1, 2, 3, 4, 5, 6]);
        assert!(smask.is_none());
    }

    #[test]
    fn gray8_wrong_length_names_expected_and_got() {
        let msg = image_message(ImageData::gray8(2, 3, vec![0; 5]));
        assert!(msg.contains('6') && msg.contains('5'), "{msg}");
    }

    #[test]
    fn rgb8_accepts_exact_length() {
        let img = ImageData::rgb8(2, 1, vec![9, 8, 7, 6, 5, 4]).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Rgb8);
        assert_eq!(pixels, [9, 8, 7, 6, 5, 4]);
        assert!(smask.is_none());
    }

    #[test]
    fn rgb8_wrong_length_names_expected_and_got() {
        let msg = image_message(ImageData::rgb8(2, 1, vec![0; 7]));
        assert!(msg.contains('6') && msg.contains('7'), "{msg}");
    }

    #[test]
    fn mono_accepts_row_padded_length() {
        let img = ImageData::mono(10, 3, vec![0; 6]).unwrap();
        assert_eq!((img.width(), img.height()), (10, 3));
        let (pixels, color, smask) = raster(&img);
        assert_eq!(color, RasterColor::Mono1);
        assert_eq!(pixels, [0; 6]);
        assert!(smask.is_none());
    }

    #[test]
    fn mono_wrong_length_names_expected_and_got() {
        let msg = image_message(ImageData::mono(10, 3, vec![0; 4]));
        assert!(msg.contains('6') && msg.contains('4'), "{msg}");
    }

    #[test]
    fn zero_dimensions_rejected() {
        image_message(ImageData::gray8(0, 3, vec![]));
        image_message(ImageData::rgb8(3, 0, vec![]));
        image_message(ImageData::mono(0, 0, vec![]));
    }

    use pdfboss_core::{Dict, Document, Name, ObjRef, Object, Stream};

    use crate::writer::{WriteOptions, Writer, XrefStyle};

    fn name(text: &str) -> Name {
        Name(text.into())
    }

    fn document_with_xobject(img: &ImageData, compress: bool) -> (Document, ObjRef) {
        let mut w = Writer::new(WriteOptions {
            xref: XrefStyle::Table,
            compress,
            object_streams: false,
            version: (1, 7),
        });
        let image_ref = img.build_xobject(&mut w);
        let content = w.put_stream(Dict::new(), b"q Q\n".to_vec());
        let pages = w.reserve();
        let mut page = Dict::new();
        page.insert(name("Type"), Object::Name(name("Page")));
        page.insert(name("Parent"), Object::Ref(pages));
        page.insert(
            name("MediaBox"),
            Object::Array(vec![
                Object::Int(0),
                Object::Int(0),
                Object::Int(100),
                Object::Int(100),
            ]),
        );
        page.insert(name("Contents"), Object::Ref(content));
        let page_ref = w.put(Object::Dict(page));
        let mut tree = Dict::new();
        tree.insert(name("Type"), Object::Name(name("Pages")));
        tree.insert(name("Kids"), Object::Array(vec![Object::Ref(page_ref)]));
        tree.insert(name("Count"), Object::Int(1));
        w.fill(pages, Object::Dict(tree)).unwrap();
        let mut catalog = Dict::new();
        catalog.insert(name("Type"), Object::Name(name("Catalog")));
        catalog.insert(name("Pages"), Object::Ref(pages));
        let root = w.put(Object::Dict(catalog));
        let doc = Document::load(w.finish(root).unwrap()).unwrap();
        (doc, image_ref)
    }

    fn xobject_stream(doc: &Document, r: ObjRef) -> Stream {
        doc.resolve(&Object::Ref(r))
            .unwrap()
            .as_stream()
            .unwrap()
            .clone()
    }

    #[test]
    fn xobject_jpeg_gray_passes_through_raw() {
        let bytes = jpeg_bytes(&[jpeg_sof(0xC0, 8, 9, 4, 1)]);
        let img = ImageData::jpeg(&bytes).unwrap();
        let (doc, image_ref) = document_with_xobject(&img, true);
        let stream = xobject_stream(&doc, image_ref);
        assert_eq!(stream.dict.get_name("Type"), Some(&name("XObject")));
        assert_eq!(stream.dict.get_name("Subtype"), Some(&name("Image")));
        assert_eq!(stream.dict.get_int("Width"), Some(9));
        assert_eq!(stream.dict.get_int("Height"), Some(4));
        assert_eq!(stream.dict.get_name("Filter"), Some(&name("DCTDecode")));
        assert_eq!(stream.dict.get_int("BitsPerComponent"), Some(8));
        assert_eq!(
            stream.dict.get_name("ColorSpace"),
            Some(&name("DeviceGray"))
        );
        assert_eq!(stream.data, bytes);
    }

    #[test]
    fn xobject_jpeg_color_uses_device_rgb() {
        let bytes = jpeg_bytes(&[jpeg_sof(0xC0, 8, 5, 7, 3)]);
        let img = ImageData::jpeg(&bytes).unwrap();
        let (doc, image_ref) = document_with_xobject(&img, true);
        let stream = xobject_stream(&doc, image_ref);
        assert_eq!(stream.dict.get_int("Width"), Some(5));
        assert_eq!(stream.dict.get_int("Height"), Some(7));
        assert_eq!(stream.dict.get_name("Filter"), Some(&name("DCTDecode")));
        assert_eq!(stream.dict.get_name("ColorSpace"), Some(&name("DeviceRGB")));
        assert_eq!(stream.data, bytes);
    }

    #[test]
    fn xobject_rgb_raster_flate_round_trips() {
        let pixels = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 8, 7];
        let img = ImageData::rgb8(2, 2, pixels.clone()).unwrap();
        let (doc, image_ref) = document_with_xobject(&img, true);
        let stream = xobject_stream(&doc, image_ref);
        assert_eq!(stream.dict.get_name("Type"), Some(&name("XObject")));
        assert_eq!(stream.dict.get_name("Subtype"), Some(&name("Image")));
        assert_eq!(stream.dict.get_int("Width"), Some(2));
        assert_eq!(stream.dict.get_int("Height"), Some(2));
        assert_eq!(stream.dict.get_name("Filter"), Some(&name("FlateDecode")));
        assert_eq!(stream.dict.get_int("BitsPerComponent"), Some(8));
        assert_eq!(stream.dict.get_name("ColorSpace"), Some(&name("DeviceRGB")));
        assert!(stream.dict.get("SMask").is_none());
        assert!(stream.dict.get("Decode").is_none());
        assert_eq!(doc.stream_data(&stream).unwrap(), pixels);
    }

    #[test]
    fn xobject_smask_is_emitted_first_as_gray8() {
        let data: [u8; 16] = [1, 2, 3, 128, 4, 5, 6, 255, 7, 8, 9, 0, 10, 11, 12, 64];
        let bytes = encode_png(
            2,
            2,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            None,
            &data,
        );
        let img = ImageData::png(&bytes).unwrap();
        let (doc, image_ref) = document_with_xobject(&img, false);
        assert_eq!(
            image_ref.num, 2,
            "the soft mask must claim the number first"
        );
        let stream = xobject_stream(&doc, image_ref);
        let mask_ref = stream.dict.get_ref("SMask").expect("SMask reference");
        assert_eq!(mask_ref.num, image_ref.num - 1);
        let mask = xobject_stream(&doc, mask_ref);
        assert_eq!(mask.dict.get_name("Type"), Some(&name("XObject")));
        assert_eq!(mask.dict.get_name("Subtype"), Some(&name("Image")));
        assert_eq!(mask.dict.get_int("Width"), Some(2));
        assert_eq!(mask.dict.get_int("Height"), Some(2));
        assert_eq!(mask.dict.get_int("BitsPerComponent"), Some(8));
        assert_eq!(mask.dict.get_name("ColorSpace"), Some(&name("DeviceGray")));
        assert_eq!(doc.stream_data(&mask).unwrap(), [128, 255, 0, 64]);
        assert_eq!(
            doc.stream_data(&stream).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        );
    }

    #[test]
    fn xobject_mono1_adds_inverted_decode() {
        let rows = vec![0b1010_1010u8; 6];
        let img = ImageData::mono(10, 3, rows.clone()).unwrap();
        let (doc, image_ref) = document_with_xobject(&img, false);
        let stream = xobject_stream(&doc, image_ref);
        assert_eq!(stream.dict.get_int("Width"), Some(10));
        assert_eq!(stream.dict.get_int("Height"), Some(3));
        assert_eq!(stream.dict.get_int("BitsPerComponent"), Some(1));
        assert_eq!(
            stream.dict.get_name("ColorSpace"),
            Some(&name("DeviceGray"))
        );
        assert_eq!(
            stream.dict.get_array("Decode"),
            Some([Object::Int(1), Object::Int(0)].as_slice())
        );
        assert_eq!(doc.stream_data(&stream).unwrap(), rows);
    }

    #[test]
    fn jpeg_rejects_degenerate_dimensions() {
        for (width, height) in [(0u16, 8u16), (8, 0), (0, 0)] {
            let bytes = jpeg_bytes(&[jpeg_sof(0xC0, 8, width, height, 3)]);
            let msg = image_message(ImageData::jpeg(&bytes));
            assert!(msg.contains("degenerate"), "{width}x{height}: {msg}");
        }
    }
}
