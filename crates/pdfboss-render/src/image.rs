//! Image XObject and inline-image decoding to RGBA (bit depths 1-16,
//! `/Decode` arrays, image masks, JPEG, Indexed lookup) and drawing via
//! inverse mapping with nearest-neighbor sampling.
//!
//! Limitation (v0.1): `/SMask` soft masks are ignored; images blend with
//! the constant fill alpha only.

use pdfboss_core::geom::{Matrix, Point, Rect};
use pdfboss_core::{Dict, Document, Object};

use crate::color::ColorSpace;
use crate::raster::Mask;
use crate::Pixmap;

/// Upper bound on decoded pixels, guarding malformed dimensions.
const MAX_PIXELS: usize = 1 << 26;
/// Upper bound on either image dimension.
const MAX_DIM: usize = 1 << 16;

/// How an image is placed and blended on the page.
pub(crate) struct DrawParams<'a> {
    /// Maps the image's unit square to device space.
    pub ctm: Matrix,
    /// Constant fill alpha (`ca`) applied to every sample.
    pub alpha: f32,
    /// Current fill color, painted through `/ImageMask` stencils.
    pub fill_rgb: [u8; 3],
    /// Active clip mask, if any.
    pub clip: Option<&'a Mask>,
}

/// A decoded RGBA image, row 0 at the image's top edge (the `v = 1` side
/// of the unit square).
struct Rgba {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

/// Decodes an image XObject or inline image and composites it onto `pix`.
///
/// `data` must already have its stream filters applied, except that a
/// trailing `DCTDecode` is passthrough (so `data` is then raw JPEG).
/// `cs_obj` is the image's `/ColorSpace` value with any resource-name
/// indirection already resolved by the caller. Undecodable images are
/// skipped (lenient).
pub(crate) fn draw(
    doc: &Document,
    pix: &mut Pixmap,
    dict: &Dict,
    data: &[u8],
    cs_obj: Option<&Object>,
    p: &DrawParams,
) {
    if let Some(img) = decode_rgba(doc, dict, data, cs_obj, p.fill_rgb) {
        draw_rgba(pix, &img, p);
    }
}

/// Reads a numeric dictionary entry, chasing references.
fn num_of(doc: &Document, dict: &Dict, key: &str) -> Option<f64> {
    doc.resolve(dict.get(key)?).ok()?.as_f64()
}

/// Reads a boolean dictionary entry, chasing references.
fn bool_of(doc: &Document, dict: &Dict, key: &str) -> Option<bool> {
    doc.resolve(dict.get(key)?).ok()?.as_bool()
}

/// Reads an array of finite numbers, chasing references at both levels.
fn floats_of(doc: &Document, dict: &Dict, key: &str) -> Option<Vec<f32>> {
    let arr = match doc.resolve(dict.get(key)?) {
        Ok(Object::Array(a)) => a,
        _ => return None,
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in &arr {
        let v = doc.resolve(item).ok()?.as_f64()? as f32;
        if !v.is_finite() {
            return None;
        }
        out.push(v);
    }
    Some(out)
}

/// Whether the last entry of the image's `/Filter` chain is `DCTDecode`
/// (whose data the stream filters pass through as raw JPEG).
fn is_dct(doc: &Document, dict: &Dict) -> bool {
    let name = match dict.get("Filter").map(|f| doc.resolve(f)) {
        Some(Ok(Object::Name(n))) => Some(n),
        Some(Ok(Object::Array(items))) => match items.last().map(|o| doc.resolve(o)) {
            Some(Ok(Object::Name(n))) => Some(n),
            _ => None,
        },
        _ => None,
    };
    matches!(
        name.as_ref().map(|n| n.0.as_str()),
        Some("DCTDecode" | "DCT")
    )
}

/// Reads the big-endian `bpc`-bit sample starting at `bit` in `data`.
/// Bits past the end of `data` read as 0 (lenient on short data).
fn sample_bits(data: &[u8], bit: usize, bpc: usize) -> u32 {
    if bpc == 8 {
        return u32::from(data.get(bit / 8).copied().unwrap_or(0));
    }
    let mut v = 0u32;
    for i in 0..bpc {
        let b = bit + i;
        let byte = data.get(b / 8).copied().unwrap_or(0);
        v = (v << 1) | u32::from((byte >> (7 - b % 8)) & 1);
    }
    v
}

/// Decodes image `dict` + `data` to RGBA. Returns `None` when the image is
/// malformed beyond recovery (bad dimensions, unsupported JPEG, ...).
fn decode_rgba(
    doc: &Document,
    dict: &Dict,
    data: &[u8],
    cs_obj: Option<&Object>,
    fill_rgb: [u8; 3],
) -> Option<Rgba> {
    if is_dct(doc, dict) {
        return decode_jpeg(data);
    }
    let width = num_of(doc, dict, "Width")? as usize;
    let height = num_of(doc, dict, "Height")? as usize;
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return None;
    }
    width.checked_mul(height).filter(|&n| n <= MAX_PIXELS)?;
    let decode = floats_of(doc, dict, "Decode");
    if bool_of(doc, dict, "ImageMask").unwrap_or(false) {
        return Some(decode_stencil(width, height, data, decode, fill_rgb));
    }
    let cs = match cs_obj {
        Some(obj) => ColorSpace::parse(doc, obj),
        None => ColorSpace::DeviceGray,
    };
    let bpc = match num_of(doc, dict, "BitsPerComponent").map(|v| v as i64) {
        Some(v @ (1 | 2 | 4 | 8 | 16)) => v as usize,
        _ => 8,
    };
    Some(decode_samples(width, height, data, &cs, bpc, decode))
}

/// Decodes a 1-bit `/ImageMask` stencil: samples that map to 0 through the
/// `/Decode` array (default `[0 1]`; `[1 0]` inverts) paint `fill_rgb`,
/// the rest stay transparent.
fn decode_stencil(
    width: usize,
    height: usize,
    data: &[u8],
    decode: Option<Vec<f32>>,
    fill_rgb: [u8; 3],
) -> Rgba {
    let invert = matches!(decode.as_deref(), Some([d0, d1, ..]) if d0 > d1);
    let stride_bits = width.div_ceil(8) * 8;
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let raw = sample_bits(data, y * stride_bits + x, 1);
            if (raw == 0) != invert {
                let off = (y * width + x) * 4;
                out[off..off + 3].copy_from_slice(&fill_rgb);
                out[off + 3] = 255;
            }
        }
    }
    Rgba {
        width,
        height,
        data: out,
    }
}

/// Decodes packed samples: per component, the raw `bpc`-bit value is mapped
/// through its `/Decode` range (default `[0 1]`, or `[0 2^bpc-1]` for
/// Indexed) and the results converted to RGB via the color space. Rows are
/// byte-aligned; missing bytes read as 0.
fn decode_samples(
    width: usize,
    height: usize,
    data: &[u8],
    cs: &ColorSpace,
    bpc: usize,
    decode: Option<Vec<f32>>,
) -> Rgba {
    let ncomp = cs.components().clamp(1, 8);
    let max = ((1u32 << bpc) - 1) as f32;
    let default_hi = if matches!(cs, ColorSpace::Indexed { .. }) {
        max
    } else {
        1.0
    };
    let ranges: Vec<(f32, f32)> = (0..ncomp)
        .map(|c| match &decode {
            Some(d) if d.len() >= 2 * (c + 1) => (d[2 * c], d[2 * c + 1]),
            _ => (0.0, default_hi),
        })
        .collect();
    let stride_bits = (ncomp * bpc * width).div_ceil(8) * 8;
    let mut out = vec![0u8; width * height * 4];
    let mut comps = [0.0f32; 8];
    for y in 0..height {
        for x in 0..width {
            let bit0 = y * stride_bits + x * ncomp * bpc;
            for (c, comp) in comps.iter_mut().enumerate().take(ncomp) {
                let raw = sample_bits(data, bit0 + c * bpc, bpc) as f32;
                let (d0, d1) = ranges[c];
                *comp = d0 + raw * (d1 - d0) / max;
            }
            let rgb = cs.to_rgb(&comps[..ncomp]);
            let off = (y * width + x) * 4;
            for (i, v) in rgb.iter().enumerate() {
                out[off + i] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            out[off + 3] = 255;
        }
    }
    Rgba {
        width,
        height,
        data: out,
    }
}

/// Decodes a raw JPEG (`DCTDecode` payload) to RGBA. Gray, RGB, and CMYK
/// pixel layouts are supported; CMYK JPEGs are assumed to carry
/// Adobe-style inverted ink values (the common case) and are un-inverted
/// before conversion. `/Decode` arrays are not applied to JPEG data.
fn decode_jpeg(data: &[u8]) -> Option<Rgba> {
    let mut dec = jpeg_decoder::Decoder::new(data);
    // The dimensions come from the JPEG's own SOF marker, not the trusted
    // PDF dictionary, so parse only the header first and validate them
    // BEFORE decode() sizes its buffers from them (a hundred-byte input
    // can otherwise claim 65535x65535 and force multi-GB allocations).
    dec.read_info().ok()?;
    let info = dec.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return None;
    }
    w.checked_mul(h).filter(|&n| n <= MAX_PIXELS)?;
    // Belt and braces: cap the decoder's internal output buffer too
    // (4 bytes/pixel covers the widest supported layout, CMYK32).
    dec.set_max_decoding_buffer_size(MAX_PIXELS * 4);
    let pixels = dec.decode().ok()?;
    let mut out = vec![255u8; w * h * 4];
    match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => {
            for (i, &g) in pixels.iter().enumerate().take(w * h) {
                out[i * 4..i * 4 + 3].copy_from_slice(&[g, g, g]);
            }
        }
        jpeg_decoder::PixelFormat::L16 => {
            for (i, pair) in pixels.chunks_exact(2).enumerate().take(w * h) {
                let g = pair[0]; // big-endian: high byte carries the tone
                out[i * 4..i * 4 + 3].copy_from_slice(&[g, g, g]);
            }
        }
        jpeg_decoder::PixelFormat::RGB24 => {
            for (i, rgb) in pixels.chunks_exact(3).enumerate().take(w * h) {
                out[i * 4..i * 4 + 3].copy_from_slice(rgb);
            }
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            for (i, cmyk) in pixels.chunks_exact(4).enumerate().take(w * h) {
                let rgb = inverted_cmyk_to_rgb([cmyk[0], cmyk[1], cmyk[2], cmyk[3]]);
                out[i * 4..i * 4 + 3].copy_from_slice(&rgb);
            }
        }
    }
    Some(Rgba {
        width: w,
        height: h,
        data: out,
    })
}

/// Converts one Adobe-inverted CMYK pixel (stored as `255 - ink`) to RGB
/// bytes with the naive `1 - min(1, x + k)` formula.
fn inverted_cmyk_to_rgb(px: [u8; 4]) -> [u8; 3] {
    let ink = |v: u8| 1.0 - f32::from(v) / 255.0;
    let rgb = ColorSpace::DeviceCMYK.to_rgb(&[ink(px[0]), ink(px[1]), ink(px[2]), ink(px[3])]);
    [
        (rgb[0] * 255.0 + 0.5) as u8,
        (rgb[1] * 255.0 + 0.5) as u8,
        (rgb[2] * 255.0 + 0.5) as u8,
    ]
}

/// Composites `rgb` at alpha `a` (0..=1) over one straight-alpha RGBA8
/// pixel using the source-over rule.
fn composite_over(dst: &mut [u8], rgb: [u8; 3], a: f32) {
    let da = f32::from(dst[3]) / 255.0;
    let oa = a + da * (1.0 - a);
    if oa <= 0.0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for i in 0..3 {
        let s = f32::from(rgb[i]);
        let d = f32::from(dst[i]);
        dst[i] = ((s * a + d * da * (1.0 - a)) / oa + 0.5) as u8;
    }
    dst[3] = (oa * 255.0 + 0.5) as u8;
}

/// Paints `img` by inverse-mapping every device pixel of the transformed
/// unit square through `p.ctm`, sampling nearest-neighbor (image row 0 at
/// the `v = 1` edge), and compositing source-over with the constant alpha
/// and clip mask.
fn draw_rgba(pix: &mut Pixmap, img: &Rgba, p: &DrawParams) {
    let Some(inv) = p.ctm.invert() else {
        return;
    };
    let alpha = if p.alpha.is_finite() {
        p.alpha.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if alpha <= 0.0 {
        return;
    }
    let bbox = Rect::new(0.0, 0.0, 1.0, 1.0).transform(p.ctm);
    let x0 = bbox.x0.floor().max(0.0) as u32;
    let y0 = bbox.y0.floor().max(0.0) as u32;
    let x1 = (bbox.x1.ceil().max(0.0) as u32).min(pix.width);
    let y1 = (bbox.y1.ceil().max(0.0) as u32).min(pix.height);
    for py in y0..y1 {
        for px in x0..x1 {
            let u = inv.apply(Point::new(px as f32 + 0.5, py as f32 + 0.5));
            if !(0.0..1.0).contains(&u.x) || !(0.0..1.0).contains(&u.y) {
                continue;
            }
            let i = ((u.x * img.width as f32) as usize).min(img.width - 1);
            let j = (((1.0 - u.y) * img.height as f32) as usize).min(img.height - 1);
            let s = &img.data[(j * img.width + i) * 4..][..4];
            let mut a = f32::from(s[3]) / 255.0 * alpha;
            if let Some(mask) = p.clip {
                a *= f32::from(mask.coverage(px, py)) / 255.0;
            }
            if a > 0.0 {
                let off = ((py * pix.width + px) * 4) as usize;
                composite_over(&mut pix.data[off..off + 4], [s[0], s[1], s[2]], a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::parser::{NoResolve, Parser};
    use pdfboss_testkit::PdfBuilder;

    fn test_doc() -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        Document::load(b.build(1)).unwrap()
    }

    fn dict(src: &[u8]) -> Dict {
        match Parser::new(src).parse_object(&NoResolve).unwrap() {
            Object::Dict(d) => d,
            other => panic!("expected dict, got {other:?}"),
        }
    }

    fn obj(src: &[u8]) -> Object {
        Parser::new(src).parse_object(&NoResolve).unwrap()
    }

    fn rgba_at(img: &Rgba, x: usize, y: usize) -> [u8; 4] {
        img.data[(y * img.width + x) * 4..][..4].try_into().unwrap()
    }

    #[test]
    fn sample_bits_all_depths() {
        let data = [0b1011_0110, 0b0101_0011];
        assert_eq!(sample_bits(&data, 0, 1), 1);
        assert_eq!(sample_bits(&data, 1, 1), 0);
        assert_eq!(sample_bits(&data, 0, 2), 0b10);
        assert_eq!(sample_bits(&data, 2, 2), 0b11);
        assert_eq!(sample_bits(&data, 0, 4), 0b1011);
        assert_eq!(sample_bits(&data, 4, 4), 0b0110);
        assert_eq!(sample_bits(&data, 0, 8), 0b1011_0110);
        assert_eq!(sample_bits(&data, 8, 8), 0b0101_0011);
        assert_eq!(sample_bits(&data, 0, 16), 0b1011_0110_0101_0011);
        // Past the end reads as zero.
        assert_eq!(sample_bits(&data, 16, 8), 0);
    }

    #[test]
    fn gray_bpc_variants_decode() {
        let doc = test_doc();
        // 2x2, 8-bit gray.
        let d = dict(b"<< /Width 2 /Height 2 /BitsPerComponent 8 >>");
        let img = decode_rgba(&doc, &d, &[0, 85, 170, 255], None, [0; 3]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [0, 0, 0, 255]);
        assert_eq!(rgba_at(&img, 1, 0), [85, 85, 85, 255]);
        assert_eq!(rgba_at(&img, 1, 1), [255, 255, 255, 255]);
        // 2x1, 1-bit gray: bits 1,0 -> white, black.
        let d = dict(b"<< /Width 2 /Height 1 /BitsPerComponent 1 >>");
        let img = decode_rgba(&doc, &d, &[0b1000_0000], None, [0; 3]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [255, 255, 255, 255]);
        assert_eq!(rgba_at(&img, 1, 0), [0, 0, 0, 255]);
        // 2x1, 4-bit gray: 0xF, 0x0.
        let d = dict(b"<< /Width 2 /Height 1 /BitsPerComponent 4 >>");
        let img = decode_rgba(&doc, &d, &[0xF0], None, [0; 3]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [255, 255, 255, 255]);
        assert_eq!(rgba_at(&img, 1, 0), [0, 0, 0, 255]);
        // 1x1, 16-bit gray mid tone.
        let d = dict(b"<< /Width 1 /Height 1 /BitsPerComponent 16 >>");
        let img = decode_rgba(&doc, &d, &[0x80, 0x00], None, [0; 3]).unwrap();
        let [r, ..] = rgba_at(&img, 0, 0);
        assert!((127..=129).contains(&r), "16-bit mid gray {r}");
    }

    #[test]
    fn rows_are_byte_aligned() {
        let doc = test_doc();
        // 3x2 1-bit gray: each row starts on its own byte.
        let d = dict(b"<< /Width 3 /Height 2 /BitsPerComponent 1 >>");
        let img = decode_rgba(&doc, &d, &[0b1010_0000, 0b0100_0000], None, [0; 3]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0)[0], 255);
        assert_eq!(rgba_at(&img, 1, 0)[0], 0);
        assert_eq!(rgba_at(&img, 2, 0)[0], 255);
        assert_eq!(rgba_at(&img, 0, 1)[0], 0);
        assert_eq!(rgba_at(&img, 1, 1)[0], 255);
        assert_eq!(rgba_at(&img, 2, 1)[0], 0);
    }

    #[test]
    fn decode_array_inverts_gray() {
        let doc = test_doc();
        let d = dict(b"<< /Width 2 /Height 1 /BitsPerComponent 8 /Decode [1 0] >>");
        let img = decode_rgba(&doc, &d, &[0, 255], None, [0; 3]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [255, 255, 255, 255], "0 inverts to 1");
        assert_eq!(rgba_at(&img, 1, 0), [0, 0, 0, 255], "255 inverts to 0");
    }

    #[test]
    fn rgb_and_cmyk_samples_decode() {
        let doc = test_doc();
        let d = dict(b"<< /Width 2 /Height 1 /BitsPerComponent 8 >>");
        let cs = obj(b"/DeviceRGB");
        let img = decode_rgba(&doc, &d, &[255, 0, 0, 0, 0, 255], Some(&cs), [0; 3]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [255, 0, 0, 255]);
        assert_eq!(rgba_at(&img, 1, 0), [0, 0, 255, 255]);

        let d = dict(b"<< /Width 1 /Height 1 /BitsPerComponent 8 >>");
        let cs = obj(b"/DeviceCMYK");
        let img = decode_rgba(&doc, &d, &[255, 0, 0, 0], Some(&cs), [0; 3]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [0, 255, 255, 255], "pure cyan");
    }

    #[test]
    fn indexed_lookup_via_palette() {
        let doc = test_doc();
        // 4-entry RGB palette, 2-bit indices: 0,1,2,3 across one row.
        let cs = obj(b"[/Indexed /DeviceRGB 3 <FF0000 00FF00 0000FF 000000>]");
        let d = dict(b"<< /Width 4 /Height 1 /BitsPerComponent 2 >>");
        let img = decode_rgba(&doc, &d, &[0b00_01_10_11], Some(&cs), [0; 3]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [255, 0, 0, 255]);
        assert_eq!(rgba_at(&img, 1, 0), [0, 255, 0, 255]);
        assert_eq!(rgba_at(&img, 2, 0), [0, 0, 255, 255]);
        assert_eq!(rgba_at(&img, 3, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn stencil_and_inverted_stencil() {
        let doc = test_doc();
        let d = dict(b"<< /Width 2 /Height 2 /ImageMask true /BitsPerComponent 1 >>");
        let img = decode_rgba(&doc, &d, &[0x40, 0x80], None, [10, 20, 30]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [10, 20, 30, 255], "0 paints");
        assert_eq!(rgba_at(&img, 1, 0), [0, 0, 0, 0], "1 transparent");
        assert_eq!(rgba_at(&img, 0, 1), [0, 0, 0, 0]);
        assert_eq!(rgba_at(&img, 1, 1), [10, 20, 30, 255]);

        let d = dict(b"<< /Width 2 /Height 2 /ImageMask true /BitsPerComponent 1 /Decode [1 0] >>");
        let img = decode_rgba(&doc, &d, &[0x40, 0x80], None, [10, 20, 30]).unwrap();
        assert_eq!(rgba_at(&img, 0, 0), [0, 0, 0, 0], "inverted: 0 transparent");
        assert_eq!(rgba_at(&img, 1, 0), [10, 20, 30, 255], "inverted: 1 paints");
    }

    /// A minimal 1x1 baseline JPEG (gray ~128): flat quant table, one-code
    /// Huffman tables, a single DC=0 block.
    fn tiny_jpeg() -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8]; // SOI
        j.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]); // DQT
        j.extend_from_slice(&[1u8; 64]);
        // SOF0: 8-bit, 1x1, one component (id 1, 1x1 sampling, table 0).
        j.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00,
        ]);
        // DHT DC0: one 1-bit code for symbol 0.
        j.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01]);
        j.extend_from_slice(&[0u8; 15]);
        j.push(0x00);
        // DHT AC0: one 1-bit code for symbol 0 (EOB).
        j.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01]);
        j.extend_from_slice(&[0u8; 15]);
        j.push(0x00);
        // SOS + entropy data: DC size 0 ("0") + EOB ("0"), padded with 1s.
        j.extend_from_slice(&[
            0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x3F,
        ]);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        j
    }

    #[test]
    fn dct_image_decodes_via_jpeg() {
        let doc = test_doc();
        let d = dict(
            b"<< /Width 1 /Height 1 /BitsPerComponent 8 /Filter /DCTDecode \
               /ColorSpace /DeviceGray >>",
        );
        let img = decode_rgba(&doc, &d, &tiny_jpeg(), None, [0; 3]).expect("jpeg decodes");
        assert_eq!((img.width, img.height), (1, 1));
        let [r, g, b, a] = rgba_at(&img, 0, 0);
        assert_eq!((r, g), (r, r), "gray");
        assert!((120..=136).contains(&r), "mid gray, got {r}");
        assert_eq!((g, b, a), (r, r, 255));
        // Garbage JPEG data is rejected, not a panic.
        assert!(decode_rgba(&doc, &d, &[1, 2, 3], None, [0; 3]).is_none());
    }

    #[test]
    fn jpeg_with_huge_sof_dimensions_is_rejected_before_decoding() {
        let doc = test_doc();
        let d = dict(
            b"<< /Width 1 /Height 1 /BitsPerComponent 8 /Filter /DCTDecode \
               /ColorSpace /DeviceGray >>",
        );
        // Same structure as tiny_jpeg() but with the SOF height/width
        // claiming 65535x65535 (~4.3e9 px, ~64x MAX_PIXELS). The pixel
        // guard must reject this from the header alone, before decode()
        // makes any dimension-sized allocation; without it, decoding
        // this 141-byte input allocates gigabytes and takes seconds.
        let mut j = tiny_jpeg();
        let sof = j.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0");
        j[sof + 5..sof + 9].copy_from_slice(&[0xFF; 4]);
        let start = std::time::Instant::now();
        assert!(decode_rgba(&doc, &d, &j, None, [0; 3]).is_none());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "header-only rejection must not attempt a decode-sized allocation"
        );
        // Zero-sized SOF dimensions are rejected too, not a panic.
        let mut j = tiny_jpeg();
        let sof = j.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0");
        j[sof + 5..sof + 9].copy_from_slice(&[0, 0, 0, 0]);
        assert!(decode_rgba(&doc, &d, &j, None, [0; 3]).is_none());
    }

    #[test]
    fn inverted_cmyk_conversion() {
        // Stored 255 everywhere = zero ink = white.
        assert_eq!(inverted_cmyk_to_rgb([255, 255, 255, 255]), [255, 255, 255]);
        // Stored 0 black channel = full black ink.
        assert_eq!(inverted_cmyk_to_rgb([255, 255, 255, 0]), [0, 0, 0]);
        // Full cyan ink only.
        assert_eq!(inverted_cmyk_to_rgb([0, 255, 255, 255]), [0, 255, 255]);
    }

    #[test]
    fn bad_dimensions_are_rejected() {
        let doc = test_doc();
        for src in [
            b"<< /Width 0 /Height 2 >>".as_slice(),
            b"<< /Height 2 >>".as_slice(),
            b"<< /Width 100000 /Height 100000 >>".as_slice(),
        ] {
            assert!(
                decode_rgba(&doc, &dict(src), &[], None, [0; 3]).is_none(),
                "{}",
                String::from_utf8_lossy(src)
            );
        }
    }

    fn quad_image() -> Rgba {
        // Row 0: red, green; row 1: blue, white.
        Rgba {
            width: 2,
            height: 2,
            data: vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 255, 255, 255, 255,
            ],
        }
    }

    fn pix_at(pix: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let off = ((y * pix.width + x) * 4) as usize;
        pix.data[off..off + 4].try_into().unwrap()
    }

    #[test]
    fn draw_maps_row_zero_to_the_v1_edge() {
        // Without a y-flip in the CTM, image row 0 (the v=1 edge) lands at
        // the bottom of the device raster (y grows downward).
        let mut pix = Pixmap::new(8, 8);
        let p = DrawParams {
            ctm: Matrix::scale(8.0, 8.0),
            alpha: 1.0,
            fill_rgb: [0; 3],
            clip: None,
        };
        draw_rgba(&mut pix, &quad_image(), &p);
        assert_eq!(pix_at(&pix, 1, 1), [0, 0, 255, 255], "row 1 left on top");
        assert_eq!(pix_at(&pix, 6, 1), [255, 255, 255, 255], "row 1 right");
        assert_eq!(pix_at(&pix, 1, 6), [255, 0, 0, 255], "row 0 left below");
        assert_eq!(pix_at(&pix, 6, 6), [0, 255, 0, 255], "row 0 right");
    }

    #[test]
    fn draw_respects_offset_alpha_and_clip() {
        let mut pix = Pixmap::new(8, 8);
        pix.fill([255, 255, 255, 255]);
        // Place the image in [4,8)x[0,4) device (translate then scale).
        let ctm = Matrix::scale(4.0, 4.0).concat(Matrix::translate(4.0, 0.0));
        let mut clip = Mask::new(8, 8);
        clip.data.iter_mut().for_each(|c| *c = 255);
        // Clip out the rightmost column.
        for y in 0..8 {
            clip.data[y * 8 + 7] = 0;
        }
        let p = DrawParams {
            ctm,
            alpha: 0.5,
            fill_rgb: [0; 3],
            clip: Some(&clip),
        };
        draw_rgba(&mut pix, &quad_image(), &p);
        assert_eq!(pix_at(&pix, 1, 1), [255, 255, 255, 255], "outside image");
        let [r, g, b, _] = pix_at(&pix, 5, 1);
        assert_eq!(b, 255, "blue keeps its own channel");
        assert!((127..=129).contains(&r), "50% blend r {r}");
        assert!((127..=129).contains(&g), "50% blend g {g}");
        assert_eq!(pix_at(&pix, 7, 1), [255, 255, 255, 255], "clipped column");
    }

    #[test]
    fn degenerate_ctm_draws_nothing() {
        let mut pix = Pixmap::new(4, 4);
        let p = DrawParams {
            ctm: Matrix::scale(0.0, 0.0),
            alpha: 1.0,
            fill_rgb: [0; 3],
            clip: None,
        };
        draw_rgba(&mut pix, &quad_image(), &p);
        assert!(pix.data.iter().all(|&b| b == 0), "pixmap untouched");
    }
}
