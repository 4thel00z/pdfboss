//! Image XObject and inline-image decoding to RGBA (bit depths 1-16,
//! `/Decode` arrays, image masks, JPEG, JPEG 2000, Indexed lookup) and
//! drawing via inverse mapping with nearest-neighbor sampling.
//!
//! Limitation (v0.1): `/SMask` and `/Mask` masking is ignored; images blend
//! with the constant fill alpha only. The executor reports every image whose
//! dictionary carries one, so the approximation is never silent. The alpha a
//! JPEG 2000 codestream carries *inside* itself (`/SMaskInData`, ISO 32000-1
//! 7.4.9) IS applied, since it arrives with the samples.

use pdfboss_core::geom::{Matrix, Point, Rect};
#[cfg(test)]
use pdfboss_core::{block_on, Document, Immediate};
use pdfboss_core::{AsyncObjectSource, Dict, Object};

use crate::color::ColorSpace;
use crate::raster::{BlendMode, Mask};
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
    /// Active blend mode; anything but `Normal` blends each sample with
    /// the backdrop pixel before compositing.
    pub blend: BlendMode,
    /// Per-sample alpha from the image's `/SMask` or `/Mask`, if any.
    pub smask: Option<&'a SampleMask>,
}

/// A decoded RGBA image, row 0 at the image's top edge (the `v = 1` side
/// of the unit square).
struct Rgba<'a> {
    width: usize,
    height: usize,
    pixels: Pixels<'a>,
    /// The stream held fewer bytes than the image's dimensions, bit depth
    /// and component count demand, so the tail of this image came from the
    /// zero padding [`sample_bits`] reads past the end of the data rather
    /// than from the image itself.
    truncated: bool,
}

/// Where an [`Rgba`] keeps its pixels.
enum Pixels<'a> {
    /// One converted RGBA quad per pixel, in row-major order.
    Quads(Vec<u8>),
    /// Packed one-component samples, still in the stream's own layout,
    /// alongside the table each sample value converts through.
    ///
    /// Converting up front would cost four bytes per source pixel, and a
    /// scan holds far more of those than the page it is drawn onto has room
    /// for: a bilevel page image of 1994 by 2832 samples occupies 690 KiB
    /// packed and 22 MiB expanded, of which a 1:1 render reads about one
    /// pixel in eighteen. So this variant converts on the way out, where the
    /// count is the destination's rather than the source's.
    Packed {
        data: &'a [u8],
        bpc: usize,
        row_bytes: usize,
        lut: Vec<[u8; 4]>,
    },
}

impl Rgba<'_> {
    /// The pixel at column `i`, row `j`, which both callers have already
    /// clamped into range.
    ///
    /// Out-of-range coordinates and samples past the end of the data yield a
    /// transparent black pixel and a zero sample respectively, which is what
    /// [`sample_bits`] does for the same reason: short data is lenient.
    fn at(&self, i: usize, j: usize) -> [u8; 4] {
        match &self.pixels {
            Pixels::Quads(data) => {
                let off = (j * self.width + i) * 4;
                data.get(off..off + 4)
                    .and_then(|s| <[u8; 4]>::try_from(s).ok())
                    .unwrap_or([0; 4])
            }
            Pixels::Packed {
                data,
                bpc,
                row_bytes,
                lut,
            } => {
                let bit = i * bpc;
                let byte = data.get(j * row_bytes + bit / 8).copied().unwrap_or(0);
                // Rows are byte-aligned (ISO 32000-1 8.9.5.2), so a sample
                // never straddles the row boundary and the shift is fixed by
                // the sample's own offset within its byte.
                let shift = 8 - bpc - bit % 8;
                let mask = ((1u16 << bpc) - 1) as u8;
                lut.get(usize::from((byte >> shift) & mask))
                    .copied()
                    .unwrap_or([0; 4])
            }
        }
    }
}

/// How much of an image [`draw`] managed to paint.
pub(crate) enum Drawn {
    /// Every sample came from the stream.
    Whole,
    /// Painted, but the sample data ended early and the rest of the image
    /// painted as zero samples.
    Truncated,
    /// Nothing painted: the image could not be decoded.
    Nothing,
    /// Nothing painted: the decode failed for a reason worth naming to the
    /// caller (a malformed JPEG 2000 codestream, a colour-space mismatch).
    Failed(String),
    /// Painted, but materially degraded: each note describes pixels the
    /// decoder lost or had to approximate.
    Degraded(Vec<String>),
}

/// Everything image decoding reads from the document, resolved up front so
/// the decode itself is pure computation over the sample data.
///
/// This split is what keeps I/O out of the pixel loops: the executor awaits
/// one metadata read, and everything after it — sample unpacking, `/Decode`
/// mapping, color conversion, compositing — runs without touching the
/// source again. The fields are read unconditionally even though a JPEG's
/// decode never looks at `/Width` and a stencil's never at `/ColorSpace`;
/// every read is lenient and pure, so the only cost is a few extra dict
/// resolves per image.
pub(crate) struct ImageMeta {
    /// The data is still a raw JPEG: the trailing `/Filter` entry is
    /// `DCTDecode`, one of the two codecs the stream filters pass through.
    dct: bool,
    /// The data is still a JPEG 2000 file or codestream: the trailing
    /// `/Filter` entry is `JPXDecode`, the other passed-through codec.
    jpx: bool,
    /// `/SMaskInData` (ISO 32000-1 Table 89, JPXDecode only): 1 or 2 asks
    /// that the codestream's own opacity channel mask the image; 2 says the
    /// colour samples are premultiplied by it. Absent reads as 0.
    smask_in_data: i64,
    width: Option<f64>,
    height: Option<f64>,
    /// A 1-bit `/ImageMask` stencil, which paints in the current fill color
    /// rather than its own.
    pub(crate) stencil: bool,
    decode: Option<Vec<f32>>,
    /// The parsed `/ColorSpace`, or `None` when the image carried none
    /// (samples then read as gray).
    cs: Option<ColorSpace>,
    bpc: Option<f64>,
}

impl ImageMeta {
    /// Reads the metadata synchronously; [`ImageMeta::read_with`] over
    /// [`Immediate`].
    #[cfg(test)]
    pub(crate) fn read(doc: &Document, dict: &Dict, cs_obj: Option<&Object>) -> ImageMeta {
        block_on(Self::read_with(&Immediate(doc), dict, cs_obj))
    }

    /// Resolves every dictionary entry the decode consults. `cs_obj` is the
    /// image's `/ColorSpace` value with any resource-name indirection
    /// already resolved by the caller.
    pub(crate) async fn read_with<S: AsyncObjectSource>(
        src: &S,
        dict: &Dict,
        cs_obj: Option<&Object>,
    ) -> ImageMeta {
        let cs = match cs_obj {
            Some(obj) => Some(ColorSpace::parse_with(src, obj).await),
            None => None,
        };
        ImageMeta {
            dct: is_dct(src, dict).await,
            jpx: is_jpx(src, dict).await,
            smask_in_data: num_of(src, dict, "SMaskInData")
                .await
                .map(|v| v as i64)
                .unwrap_or(0),
            width: num_of(src, dict, "Width").await,
            height: num_of(src, dict, "Height").await,
            stencil: bool_of(src, dict, "ImageMask").await.unwrap_or(false),
            decode: floats_of(src, dict, "Decode").await,
            cs,
            bpc: num_of(src, dict, "BitsPerComponent").await,
        }
    }
}

/// Decodes an image XObject or inline image and composites it onto `pix`.
///
/// `data` must already have its stream filters applied, except for a
/// trailing `DCTDecode` or `JPXDecode`: those are the two codecs the filter
/// chain passes through (ISO 32000-1 7.4.9), leaving `data` a raw JPEG or a
/// JPEG 2000 file, which this module decodes. Every other codec is rejected
/// there, so `data` never arrives as a codestream this module would
/// otherwise read as samples.
/// Undecodable images are skipped (lenient); the return value says what was
/// painted, so the caller can record the miss.
pub(crate) fn draw(pix: &mut Pixmap, meta: &ImageMeta, data: &[u8], p: &DrawParams) -> Drawn {
    if meta.jpx {
        return draw_jpx(pix, meta, data, p);
    }
    match decode_rgba(meta, data, p.fill_rgb) {
        Some(img) => {
            let truncated = img.truncated;
            draw_rgba(pix, &img, p);
            if truncated {
                Drawn::Truncated
            } else {
                Drawn::Whole
            }
        }
        None => Drawn::Nothing,
    }
}

/// Reads a numeric dictionary entry, chasing references.
async fn num_of<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<f64> {
    src.resolve(dict.get(key)?).await.ok()?.as_f64()
}

/// Reads a boolean dictionary entry, chasing references.
async fn bool_of<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<bool> {
    src.resolve(dict.get(key)?).await.ok()?.as_bool()
}

/// Reads an array of finite numbers, chasing references at both levels.
async fn floats_of<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<Vec<f32>> {
    let arr = match src.resolve(dict.get(key)?).await {
        Ok(Object::Array(a)) => a,
        _ => return None,
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in &arr {
        let v = src.resolve(item).await.ok()?.as_f64()? as f32;
        if !v.is_finite() {
            return None;
        }
        out.push(v);
    }
    Some(out)
}

/// Whether the trailing filter of the image's `/Filter` chain is
/// `DCTDecode` (whose data the stream filters pass through as raw JPEG).
/// "Trailing" is read by the shared core helper, which skips non-Name
/// entries exactly as `decode_stream` does — `[/DCTDecode null]` is still
/// a passthrough.
async fn is_dct<S: AsyncObjectSource>(src: &S, dict: &Dict) -> bool {
    matches!(
        pdfboss_core::filters::trailing_filter_with(src, dict)
            .await
            .as_ref()
            .map(|n| n.0.as_str()),
        Some("DCTDecode" | "DCT")
    )
}

/// Whether the trailing filter of the image's `/Filter` chain is
/// `JPXDecode` (whose data the stream filters pass through as a JPEG 2000
/// file or raw codestream), read like [`is_dct`]. There is no abbreviated
/// form: ISO 32000-1 Table 94 defines none, JPXDecode not being an
/// inline-image filter.
async fn is_jpx<S: AsyncObjectSource>(src: &S, dict: &Dict) -> bool {
    matches!(
        pdfboss_core::filters::trailing_filter_with(src, dict)
            .await
            .as_ref()
            .map(|n| n.0.as_str()),
        Some("JPXDecode")
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

/// Whether `data` is shorter than the `height` rows of `row_bits` each that
/// the image demands: [`sample_bits`] pads the shortfall with zero bits, so
/// the missing region paints as if the stream had said "black" (or "clear",
/// for a stencil) rather than as the image's own samples. `row_bits` is
/// already rounded up to whole bytes by both callers.
fn short_of_samples(data: &[u8], row_bits: usize, height: usize) -> bool {
    data.len() < (row_bits / 8).saturating_mul(height)
}

/// Per-sample alpha applied to a base image at draw time: an `/SMask`'s
/// gray samples, a `/Mask` stencil's paintable bits, or the inverse of a
/// color-key `/Mask`'s matches — sampled nearest-neighbor in the image's
/// unit square, so a mask whose dimensions differ from the base's still
/// lands on the right pixels (§8.9.6.3).
pub(crate) struct SampleMask {
    width: usize,
    height: usize,
    /// Row-major alpha, row 0 at the image's top edge.
    data: Vec<u8>,
}

impl SampleMask {
    /// The mask's alpha at unit-square coordinates (`u` right, `v` up).
    fn sample(&self, u: f32, v: f32) -> u8 {
        let i = ((u * self.width as f32) as usize).min(self.width - 1);
        let j = (((1.0 - v) * self.height as f32) as usize).min(self.height - 1);
        self.data[j * self.width + i]
    }
}

/// Decodes a mask image into per-sample alpha: an `/SMask`'s luminance
/// (its gray sample IS the alpha, `/Decode` applied), or a `/Mask`
/// stencil's paintable bits (sample 1 hides the base sample, §8.9.6.4 —
/// which is exactly the stencil decode's transparent side).
pub(crate) fn decode_alpha(meta: &ImageMeta, data: &[u8]) -> Option<SampleMask> {
    let img = decode_rgba(meta, data, [255, 255, 255])?;
    let (width, height) = (img.width, img.height);
    if width == 0 || height == 0 {
        return None;
    }
    let mut out = vec![0u8; width.checked_mul(height)?];
    for j in 0..height {
        for i in 0..width {
            let px = img.at(i, j);
            out[j * width + i] = if meta.stencil { px[3] } else { px[0] };
        }
    }
    Some(SampleMask {
        width,
        height,
        data: out,
    })
}

/// Builds the alpha a color-key `/Mask` array describes: a sample whose
/// every raw component lies inside its `[min, max]` range is transparent
/// (§8.9.6.4). `None` when the base's samples cannot be walked here (a
/// passed-through JPEG or JPEG 2000 codestream, or a stencil).
pub(crate) fn color_key_mask(meta: &ImageMeta, data: &[u8], key: &[i64]) -> Option<SampleMask> {
    if meta.dct || meta.jpx || meta.stencil {
        return None;
    }
    let width = meta.width? as usize;
    let height = meta.height? as usize;
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return None;
    }
    width.checked_mul(height).filter(|&n| n <= MAX_PIXELS)?;
    let cs = meta.cs.as_ref().unwrap_or(&ColorSpace::DeviceGray);
    let ncomp = cs.components().clamp(1, 8);
    if key.len() < 2 * ncomp {
        return None;
    }
    let bpc = match meta.bpc.map(|v| v as i64) {
        Some(v @ (1 | 2 | 4 | 8 | 16)) => v as usize,
        _ => 8,
    };
    let stride_bits = (ncomp * bpc * width).div_ceil(8) * 8;
    let mut out = vec![0u8; width * height];
    for y in 0..height {
        for x in 0..width {
            let base = y * stride_bits + x * ncomp * bpc;
            let inside = (0..ncomp).all(|c| {
                let v = i64::from(sample_bits(data, base + c * bpc, bpc));
                v >= key[2 * c] && v <= key[2 * c + 1]
            });
            out[y * width + x] = if inside { 0 } else { 255 };
        }
    }
    Some(SampleMask {
        width,
        height,
        data: out,
    })
}

/// Decodes an image's `data` to RGBA under its resolved metadata. Pure —
/// every document read happened in [`ImageMeta::read_with`]. Returns `None`
/// when the image is malformed beyond recovery (bad dimensions, unsupported
/// JPEG, ...).
fn decode_rgba<'a>(meta: &ImageMeta, data: &'a [u8], fill_rgb: [u8; 3]) -> Option<Rgba<'a>> {
    if meta.dct {
        // A short JPEG is the decoder's business: it either reconstructs
        // what it has or fails outright, and there is no zero padding of
        // ours to own up to.
        return decode_jpeg(data);
    }
    let width = meta.width? as usize;
    let height = meta.height? as usize;
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return None;
    }
    width.checked_mul(height).filter(|&n| n <= MAX_PIXELS)?;
    let decode = meta.decode.as_deref();
    if meta.stencil {
        return Some(decode_stencil(width, height, data, decode, fill_rgb));
    }
    let cs = meta.cs.as_ref().unwrap_or(&ColorSpace::DeviceGray);
    let bpc = match meta.bpc.map(|v| v as i64) {
        Some(v @ (1 | 2 | 4 | 8 | 16)) => v as usize,
        _ => 8,
    };
    Some(decode_samples(width, height, data, cs, bpc, decode))
}

/// Decodes a 1-bit `/ImageMask` stencil: samples that map to 0 through the
/// `/Decode` array (default `[0 1]`; `[1 0]` inverts) paint `fill_rgb`,
/// the rest stay transparent.
fn decode_stencil(
    width: usize,
    height: usize,
    data: &[u8],
    decode: Option<&[f32]>,
    fill_rgb: [u8; 3],
) -> Rgba<'static> {
    let invert = matches!(decode, Some([d0, d1, ..]) if d0 > d1);
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
        pixels: Pixels::Quads(out),
        truncated: short_of_samples(data, stride_bits, height),
    }
}

/// Decodes packed samples: per component, the raw `bpc`-bit value is mapped
/// through its `/Decode` range (default `[0 1]`, or `[0 2^bpc-1]` for
/// Indexed) and the results converted to RGB via the color space. Rows are
/// byte-aligned; missing bytes read as 0.
fn decode_samples<'a>(
    width: usize,
    height: usize,
    data: &'a [u8],
    cs: &ColorSpace,
    bpc: usize,
    decode: Option<&[f32]>,
) -> Rgba<'a> {
    let ncomp = cs.components().clamp(1, 8);
    let max = ((1u32 << bpc) - 1) as f32;
    let default_hi = if matches!(cs, ColorSpace::Indexed { .. }) {
        max
    } else {
        1.0
    };
    let ranges: Vec<(f32, f32)> = (0..ncomp)
        .map(|c| match decode {
            Some(d) if d.len() >= 2 * (c + 1) => (d[2 * c], d[2 * c + 1]),
            _ => (0.0, default_hi),
        })
        .collect();
    let stride_bits = (ncomp * bpc * width).div_ceil(8) * 8;
    let truncated = short_of_samples(data, stride_bits, height);
    if ncomp == 1 && bpc <= 8 {
        // One component of at most eight bits admits at most 256 distinct
        // samples, so `/Decode` and the color conversion run once per value
        // rather than once per pixel, and the samples themselves are left
        // packed for [`Rgba::at`] to read. Bilevel scans are the extreme
        // case: two conversions replace one per pixel, over data that is
        // never expanded at all.
        return Rgba {
            width,
            height,
            pixels: Pixels::Packed {
                data,
                bpc,
                row_bytes: (width * bpc).div_ceil(8),
                lut: sample_lut(cs, bpc, ranges[0], max),
            },
            truncated,
        };
    }
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
        pixels: Pixels::Quads(out),
        truncated,
    }
}

/// The opaque RGBA pixel each raw sample value of a one-component image
/// decodes to, indexed by that value. Built with the same `/Decode` mapping
/// and color conversion the general path applies per pixel, so the two agree
/// pixel for pixel.
fn sample_lut(cs: &ColorSpace, bpc: usize, range: (f32, f32), max: f32) -> Vec<[u8; 4]> {
    let (d0, d1) = range;
    (0..1usize << bpc)
        .map(|raw| {
            let rgb = cs.to_rgb(&[d0 + raw as f32 * (d1 - d0) / max]);
            let mut px = [0u8, 0, 0, 255];
            for (slot, v) in px.iter_mut().zip(rgb) {
                *slot = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            px
        })
        .collect()
}

/// Decodes a raw JPEG (`DCTDecode` payload) to RGBA. Gray, RGB, and CMYK
/// pixel layouts are supported; CMYK JPEGs are assumed to carry
/// Adobe-style inverted ink values (the common case) and are un-inverted
/// before conversion. `/Decode` arrays are not applied to JPEG data.
fn decode_jpeg<'a>(data: &[u8]) -> Option<Rgba<'a>> {
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
        pixels: Pixels::Quads(out),
        truncated: false,
    })
}

/// JPEG 2000 decode bounds mapped from this module's own image guards: the
/// same pixel cap ([`MAX_PIXELS`]), a component count bounded by what a PDF
/// colour space can consume (`ColorSpace::components` is clamped to 8) plus
/// one opacity channel, and decoded bytes bounded by the RGBA expansion
/// this module allocates anyway (4 bytes per pixel at the pixel cap).
fn jpx_limits() -> pdfboss_jpx::DecodeLimits {
    pdfboss_jpx::DecodeLimits {
        max_pixels: MAX_PIXELS as u64,
        max_components: 9,
        max_decoded_bytes: (MAX_PIXELS as u64) * 4,
        ..pdfboss_jpx::DecodeLimits::default()
    }
}

/// Decodes and paints a `JPXDecode` image. Failures skip the image with a
/// named reason; decoder warnings that cost pixels surface as degradation
/// notes on an image that still painted. Under `/ImageMask true` the
/// decoded channel is a stencil painting the current fill colour
/// (ISO 32000-1 7.4.9), never the image's own.
fn draw_jpx(pix: &mut Pixmap, meta: &ImageMeta, data: &[u8], p: &DrawParams) -> Drawn {
    let decoded = match pdfboss_jpx::decode(data, &jpx_limits()) {
        Ok(decoded) => decoded,
        Err(e) => return Drawn::Failed(format!("JPXDecode: {e}")),
    };
    let converted = if meta.stencil {
        jpx_stencil(meta, &decoded, p.fill_rgb)
    } else {
        jpx_rgba(meta, decoded)
    };
    let (img, notes) = match converted {
        Ok(converted) => converted,
        Err(reason) => return Drawn::Failed(reason),
    };
    draw_rgba(pix, &img, p);
    if notes.is_empty() {
        Drawn::Whole
    } else {
        Drawn::Degraded(notes)
    }
}

/// The dimension and sample-count guards shared by the image and stencil
/// interpretations of a decoded codestream. `Err` is a reason to skip.
fn jpx_dimensions(img: &pdfboss_jpx::DecodedImage) -> Result<(usize, usize), String> {
    let width = img.width as usize;
    let height = img.height as usize;
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return Err(format!("JPXDecode: bad dimensions {width}x{height}"));
    }
    width
        .checked_mul(height)
        .filter(|&n| n <= MAX_PIXELS)
        .ok_or_else(|| format!("JPXDecode: {width}x{height} exceeds the pixel cap"))?;
    let components = usize::from(img.components);
    if components == 0 || img.samples.len() < width * height * components {
        return Err("JPXDecode: the decoder returned too few samples".to_string());
    }
    Ok((width, height))
}

/// Interprets a decoded codestream under `/ImageMask true` — ISO 32000-1
/// 7.4.9: "If the image is a stencil mask [...] the JPEG2000 data shall
/// provide a single colour channel with 1-bit samples." The channel is
/// repacked to the 1-bit layout [`decode_stencil`] reads, so the stencil
/// semantics — `/Decode`, which 7.4.9 keeps for masks alone, selecting
/// whether a 0 or a 1 sample paints `fill_rgb` — are exactly the shared
/// ones. A conforming 1-bit channel normalizes to samples of exactly 0
/// or 255; the midpoint threshold is exact for those and still sensible
/// for a (malformed) deeper channel. More than one channel cannot be a
/// stencil at all: a named failure, and the caller skips the image.
fn jpx_stencil(
    meta: &ImageMeta,
    img: &pdfboss_jpx::DecodedImage,
    fill_rgb: [u8; 3],
) -> Result<(Rgba<'static>, Vec<String>), String> {
    let (width, height) = jpx_dimensions(img)?;
    if img.components != 1 {
        return Err(format!(
            "JPXDecode: /ImageMask true but the codestream carries {} channels \
             where ISO 32000-1 7.4.9 demands a single one",
            img.components
        ));
    }
    let notes = material_jpx_warnings(&img.warnings);
    let row_bytes = width.div_ceil(8);
    let mut packed = vec![0u8; row_bytes * height];
    for y in 0..height {
        for x in 0..width {
            if img.samples[y * width + x] >= 128 {
                packed[y * row_bytes + x / 8] |= 128 >> (x % 8);
            }
        }
    }
    Ok((
        decode_stencil(width, height, &packed, meta.decode.as_deref(), fill_rgb),
        notes,
    ))
}

/// Interprets a decoded JPEG 2000 image under the PDF image dictionary
/// (ISO 32000-1 7.4.9):
///
/// - the dict's `/ColorSpace`, when present, overrides the codestream's
///   colour declaration, and its component count must match the colour
///   channels the codestream carries; an `/Indexed` space additionally has
///   the decoder's 8-bit normalization reversed (via
///   [`DecodedImage::component_depths`]) so its samples index the palette
///   exactly;
/// - without one, the codestream's declaration maps to the matching device
///   space, or — for a declaration the decoder does not interpret (an ICC
///   profile, an unconverted enumeration) — is approximated by channel
///   count, with a note saying so;
/// - a channel the codestream marks as opacity is never a colour channel;
///   `/SMaskInData` 1 or 2 additionally turns it into per-pixel alpha, and
///   2 un-premultiplies the colour samples by it first;
/// - `/Decode` is ignored — 7.4.9: "Decode shall be ignored, except in the
///   case where the image is treated as a mask" — and so is the dict's
///   `/BitsPerComponent`: the decoder already normalized every sample to
///   8 bits.
///
/// `Err` is a reason to skip the image. The `Vec<String>` collects material
/// degradation: decoder warnings that cost pixels, plus this function's own
/// approximation notes.
fn jpx_rgba(
    meta: &ImageMeta,
    img: pdfboss_jpx::DecodedImage,
) -> Result<(Rgba<'static>, Vec<String>), String> {
    let (width, height) = jpx_dimensions(&img)?;
    let components = usize::from(img.components);
    let alpha = img.alpha_index.map(usize::from).filter(|&a| a < components);
    let color_count = components - usize::from(alpha.is_some());
    if color_count == 0 {
        return Err("JPXDecode: the codestream carries only an opacity channel".to_string());
    }

    let mut notes = material_jpx_warnings(&img.warnings);

    let mapped;
    let cs: &ColorSpace = match meta.cs.as_ref() {
        Some(cs) => {
            // The image dictionary wins over the codestream (7.4.9), but it
            // can only reinterpret the channels that are there.
            if cs.components() != color_count {
                return Err(format!(
                    "JPXDecode: /ColorSpace expects {} component(s) but the codestream \
                     carries {color_count} colour channel(s)",
                    cs.components()
                ));
            }
            cs
        }
        None => {
            mapped = match img.color {
                pdfboss_jpx::ColorKind::Gray => ColorSpace::DeviceGray,
                pdfboss_jpx::ColorKind::Rgb => ColorSpace::DeviceRGB,
                pdfboss_jpx::ColorKind::Cmyk => ColorSpace::DeviceCMYK,
                // An ICC profile or an enumeration the decoder does not
                // convert (T.800 I.5.3.3): approximate by channel count,
                // and own up to the guess.
                _ => {
                    notes.push(format!(
                        "JPXDecode: colour approximated from {color_count} channel(s); \
                         the codestream's colour declaration is not interpreted"
                    ));
                    match color_count {
                        1 => ColorSpace::DeviceGray,
                        3 => ColorSpace::DeviceRGB,
                        4 => ColorSpace::DeviceCMYK,
                        n => {
                            return Err(format!(
                                "JPXDecode: no colour space for {n} colour channel(s)"
                            ))
                        }
                    }
                }
            };
            if mapped.components() != color_count {
                return Err(format!(
                    "JPXDecode: the codestream declares {} colour but carries \
                     {color_count} colour channel(s)",
                    mapped.components()
                ));
            }
            &mapped
        }
    };

    // A marked opacity channel never reaches the colour conversion; it
    // becomes per-pixel alpha only when /SMaskInData asks for masking.
    let masking = alpha.is_some() && matches!(meta.smask_in_data, 1 | 2);
    let premultiplied = masking && meta.smask_in_data == 2;
    let mut color_data = Vec::with_capacity(width * height * color_count);
    let mut alpha_data = Vec::with_capacity(if masking { width * height } else { 0 });
    let mut zero_alpha_color = false;
    for px in img.samples.chunks_exact(components) {
        let a = alpha.map_or(255, |i| px[i]);
        for (i, &s) in px.iter().enumerate() {
            if Some(i) == alpha {
                continue;
            }
            color_data.push(if premultiplied {
                // 7.4.9: stored colour is colour x alpha; divide it back
                // out. At alpha 0 there is no colour to recover — a nonzero
                // sample there is malformed and clamps to 0, once noted.
                if a == 0 {
                    zero_alpha_color |= s != 0;
                    0
                } else {
                    ((u32::from(s) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
                }
            } else {
                s
            });
        }
        if masking {
            alpha_data.push(a);
        }
    }
    if zero_alpha_color {
        notes.push(
            "JPXDecode: premultiplied colour under zero opacity clamped to zero \
             (/SMaskInData 2)"
                .to_string(),
        );
    }

    // An `/Indexed` colour space consumes the samples as PALETTE INDICES
    // (ISO 32000-1 7.4.9 allows it over JPX like any other space), but the
    // decoder normalized every channel to 8 bits — T.800 knows nothing of
    // PDF palettes — which rewrites an index: depths below 8 scale by
    // 255/(2^d - 1), depths above 8 drop their low bits. The scaling is
    // injective, so [`jpx_palette_index`] recovers the exact index; the
    // dropped bits of a deeper channel are gone — legal indices fit 8 bits
    // anyway (hival <= 255, ISO 32000-1 8.6.6.3) — so those samples pass
    // through with a note owning up to the approximation.
    if matches!(cs, ColorSpace::Indexed { .. }) {
        // Indexed has one component, so the one colour channel's position
        // is 0 unless the opacity channel sits there.
        let channel = usize::from(alpha == Some(0));
        let depth = img.component_depths.get(channel).copied().unwrap_or(8);
        if depth < 8 {
            for v in &mut color_data {
                *v = jpx_palette_index(*v, depth);
            }
        } else if depth > 8 {
            notes.push(format!(
                "JPXDecode: palette indices carried in a {depth}-bit channel lost \
                 their low bits in the 8-bit normalization; the palette lookup is \
                 approximate"
            ));
        }
    }

    // The same colour conversion every other image gets, at the decoder's
    // normalized 8 bits per component — but with no `/Decode` mapping:
    // ISO 32000-1 7.4.9 says "Decode shall be ignored, except in the case
    // where the image is treated as a mask", and this function never
    // handles the mask case.
    let converted = decode_samples(width, height, &color_data, cs, 8, None);
    let mut quads = owned_quads(converted);
    if masking {
        for (px, &a) in quads.chunks_exact_mut(4).zip(&alpha_data) {
            px[3] = a;
        }
    }
    Ok((
        Rgba {
            width,
            height,
            pixels: Pixels::Quads(quads),
            truncated: false,
        },
        notes,
    ))
}

/// The palette index a decoded 8-bit sample was normalized FROM, for a
/// source depth below 8. The decoder scales an index `i` to
/// `round(i * 255 / (2^depth - 1))` (its documented contract for shallow
/// channels), and the step between neighbouring indices — at least
/// 255/127 for depth 7 — always exceeds 1, so the scaling is injective
/// and rounding back with `round(v * (2^depth - 1) / 255)` recovers `i`
/// exactly; `jpx_palette_index_reverses_the_normalization_exactly`
/// proves it for every value of every depth.
fn jpx_palette_index(v: u8, depth: u8) -> u8 {
    let max = (1u32 << depth) - 1;
    ((u32::from(v) * max + 127) / 255) as u8
}

/// Expands a decoded image to one owned RGBA quad per pixel. The general
/// path already is that; the packed one-component path converts through its
/// own lookup table, so the two agree pixel for pixel.
fn owned_quads(img: Rgba<'_>) -> Vec<u8> {
    if let Pixels::Quads(quads) = img.pixels {
        return quads;
    }
    let mut out = vec![0u8; img.width * img.height * 4];
    for y in 0..img.height {
        for x in 0..img.width {
            let off = (y * img.width + x) * 4;
            out[off..off + 4].copy_from_slice(&img.at(x, y));
        }
    }
    out
}

/// The decoder warnings that mean pixels were actually lost or misread — a
/// corrupt code-block, a tile or channel zeroed, a skipped component
/// transform — as opposed to advisory notes about tolerated stream quirks
/// (the TNsot tile-part count, a skipped rreq box), which would only be
/// noise in a render report. The split is the decoder's own
/// [`pdfboss_jpx::JpxWarning::data_loss`] contract; the message text is
/// free-form and never consulted.
fn material_jpx_warnings(warnings: &[pdfboss_jpx::JpxWarning]) -> Vec<String> {
    warnings
        .iter()
        .filter(|w| w.data_loss)
        .map(|w| format!("JPXDecode: {}", w.message))
        .collect()
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
fn draw_rgba(pix: &mut Pixmap, img: &Rgba<'_>, p: &DrawParams) {
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
            let s = img.at(i, j);
            let mut a = f32::from(s[3]) / 255.0 * alpha;
            if let Some(m) = p.smask {
                a *= f32::from(m.sample(u.x, u.y)) / 255.0;
            }
            if let Some(mask) = p.clip {
                a *= f32::from(mask.coverage(px, py)) / 255.0;
            }
            if a <= 0.0 {
                continue;
            }
            let off = ((py * pix.width + px) * 4) as usize;
            let dst = &mut pix.data[off..off + 4];
            let s = if p.blend == BlendMode::Normal {
                s
            } else {
                let b = p.blend.blend([dst[0], dst[1], dst[2]], [s[0], s[1], s[2]]);
                [b[0], b[1], b[2], s[3]]
            };
            if a >= 1.0 && p.blend == BlendMode::Normal {
                // An opaque source covers whatever is under it: source-over
                // reduces to a copy. Taking it through the general formula
                // would spend three divides to arrive at these same four
                // bytes, and a scanned page is opaque over its whole extent.
                dst.copy_from_slice(&[s[0], s[1], s[2], 255]);
            } else if a >= 1.0 {
                dst.copy_from_slice(&[s[0], s[1], s[2], 255]);
            } else {
                composite_over(dst, [s[0], s[1], s[2]], a);
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

    fn rgba_at(img: &Rgba<'_>, x: usize, y: usize) -> [u8; 4] {
        img.at(x, y)
    }

    /// The pre-split decode signature, kept so every assertion below stays
    /// byte-identical: metadata resolution and the pure decode are exercised
    /// together, exactly as `draw` composes them.
    fn decode_rgba<'a>(
        doc: &Document,
        dict: &Dict,
        data: &'a [u8],
        cs_obj: Option<&Object>,
        fill_rgb: [u8; 3],
    ) -> Option<Rgba<'a>> {
        super::decode_rgba(&ImageMeta::read(doc, dict, cs_obj), data, fill_rgb)
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
    fn the_lookup_table_paints_what_per_pixel_conversion_would() {
        // The table is only an optimization for one-component images, so it
        // must agree with the general path's formula everywhere -- including
        // past the end of short data, where samples read as 0, and at a
        // width that leaves spare bits in the last byte of every row.
        let cs = ColorSpace::DeviceGray;
        let width = 13;
        let height = 3;
        for bpc in [1usize, 2, 4, 8] {
            let row_bytes = (width * bpc).div_ceil(8);
            let full: Vec<u8> = (0..row_bytes * height)
                .map(|i| (i as u8).wrapping_mul(37).wrapping_add(11))
                .collect();
            // The truncated case stops mid-row, so the padding starts at a
            // sample boundary that is not a row boundary.
            for data in [&full[..], &full[..row_bytes + 1]] {
                for range in [(0.0f32, 1.0f32), (1.0, 0.0), (0.25, 0.75)] {
                    let max = ((1u32 << bpc) - 1) as f32;
                    let decode = [range.0, range.1];
                    let got = decode_samples(width, height, data, &cs, bpc, Some(&decode));

                    let stride_bits = row_bytes * 8;
                    for y in 0..height {
                        for x in 0..width {
                            let raw = sample_bits(data, y * stride_bits + x * bpc, bpc) as f32;
                            let (d0, d1) = range;
                            let rgb = cs.to_rgb(&[d0 + raw * (d1 - d0) / max]);
                            let mut want = [0u8, 0, 0, 255];
                            for (slot, v) in want.iter_mut().zip(rgb) {
                                *slot = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                            }
                            assert_eq!(
                                got.at(x, y),
                                want,
                                "bpc {bpc} range {range:?} at ({x},{y}) \
                                 with {} bytes",
                                data.len()
                            );
                        }
                    }
                }
            }
        }
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
        let jpeg = tiny_jpeg();
        let img = decode_rgba(&doc, &d, &jpeg, None, [0; 3]).expect("jpeg decodes");
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

    /// A decoded JPEG 2000 image as the `pdfboss-jpx` crate would hand it
    /// over, with no warnings; the interpretation tests below drive
    /// [`jpx_rgba`] with these directly, so every dictionary combination is
    /// exercised without a codestream fixture per case.
    fn jpx_image(
        width: u32,
        height: u32,
        components: u8,
        samples: Vec<u8>,
        color: pdfboss_jpx::ColorKind,
        alpha_index: Option<u8>,
    ) -> pdfboss_jpx::DecodedImage {
        pdfboss_jpx::DecodedImage {
            width,
            height,
            components,
            samples,
            component_depths: vec![8; usize::from(components)],
            color,
            alpha_index,
            warnings: Vec::new(),
        }
    }

    /// An [`ImageMeta`] resolved from dictionary source, as the executor
    /// builds it for a JPX image.
    fn jpx_meta(doc: &Document, dict_src: &[u8], cs_src: Option<&[u8]>) -> ImageMeta {
        let cs = cs_src.map(obj);
        ImageMeta::read(doc, &dict(dict_src), cs.as_ref())
    }

    #[test]
    fn jpx_gray_maps_to_device_gray_and_ignores_decode() {
        let doc = test_doc();
        let image = || jpx_image(2, 1, 1, vec![0, 255], pdfboss_jpx::ColorKind::Gray, None);

        let meta = jpx_meta(&doc, b"<< >>", None);
        let (img, notes) = jpx_rgba(&meta, image()).expect("gray decodes");
        assert!(notes.is_empty(), "no degradation: {notes:?}");
        assert_eq!(rgba_at(&img, 0, 0), [0, 0, 0, 255]);
        assert_eq!(rgba_at(&img, 1, 0), [255, 255, 255, 255]);

        // ISO 32000-1 7.4.9: "Decode shall be ignored, except in the case
        // where the image is treated as a mask" — the `[1 0]` array must
        // NOT invert a JPX image. `/BitsPerComponent 1` is ignored too:
        // the samples are the decoder's normalized 8-bit.
        let meta = jpx_meta(&doc, b"<< /Decode [1 0] /BitsPerComponent 1 >>", None);
        let (img, _) = jpx_rgba(&meta, image()).expect("gray decodes");
        assert_eq!(rgba_at(&img, 0, 0), [0, 0, 0, 255]);
        assert_eq!(rgba_at(&img, 1, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn jpx_dict_colorspace_overrides_the_codestream() {
        // The codestream declares an enumeration nothing converts; the
        // dictionary's /DeviceRGB wins (ISO 32000-1 7.4.9) and, being
        // authoritative, is no guess: no note.
        let doc = test_doc();
        let color = pdfboss_jpx::ColorKind::Other {
            enumeration: 9,
            components: 3,
        };
        let image = jpx_image(1, 1, 3, vec![255, 0, 10], color, None);
        let meta = jpx_meta(&doc, b"<< >>", Some(b"/DeviceRGB"));
        let (img, notes) = jpx_rgba(&meta, image).expect("rgb decodes");
        assert!(notes.is_empty(), "an override is not a guess: {notes:?}");
        assert_eq!(rgba_at(&img, 0, 0), [255, 0, 10, 255]);
    }

    #[test]
    fn jpx_colorspace_component_mismatch_is_a_named_failure() {
        let doc = test_doc();
        let image = jpx_image(1, 1, 1, vec![7], pdfboss_jpx::ColorKind::Gray, None);
        let meta = jpx_meta(&doc, b"<< >>", Some(b"/DeviceRGB"));
        let reason = match jpx_rgba(&meta, image) {
            Err(reason) => reason,
            Ok(..) => panic!("1 channel is not RGB"),
        };
        assert!(
            reason.contains("3 component(s)") && reason.contains("1 colour channel(s)"),
            "{reason}"
        );
    }

    #[test]
    fn jpx_uninterpreted_color_is_approximated_by_channel_count_with_a_note() {
        let doc = test_doc();
        let color = pdfboss_jpx::ColorKind::IccGuess { components: 3 };
        let image = jpx_image(1, 1, 3, vec![0, 255, 0], color, None);
        let meta = jpx_meta(&doc, b"<< >>", None);
        let (img, notes) = jpx_rgba(&meta, image).expect("3 channels read as RGB");
        assert_eq!(rgba_at(&img, 0, 0), [0, 255, 0, 255]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("colour approximated"), "{notes:?}");
    }

    #[test]
    fn jpx_alpha_channel_masks_only_when_smask_in_data_asks() {
        let doc = test_doc();
        // One RGBA pixel: red at half opacity, alpha in channel 3.
        let image = || {
            jpx_image(
                1,
                1,
                4,
                vec![255, 0, 0, 128],
                pdfboss_jpx::ColorKind::Rgb,
                Some(3),
            )
        };

        // /SMaskInData absent: the opacity channel still never reaches the
        // colour conversion (the pixel is red, not a 4-channel misread),
        // but it does not mask either (ISO 32000-1 Table 89, value 0).
        let meta = jpx_meta(&doc, b"<< >>", None);
        let (img, _) = jpx_rgba(&meta, image()).expect("decodes");
        assert_eq!(rgba_at(&img, 0, 0), [255, 0, 0, 255]);

        // /SMaskInData 1: the channel becomes per-pixel alpha as stored.
        let meta = jpx_meta(&doc, b"<< /SMaskInData 1 >>", None);
        let (img, notes) = jpx_rgba(&meta, image()).expect("decodes");
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(rgba_at(&img, 0, 0), [255, 0, 0, 128]);
    }

    #[test]
    fn jpx_premultiplied_color_is_divided_back_out() {
        let doc = test_doc();
        // Premultiplied half-opacity red: stored colour = 255 * 128 / 255.
        let image = jpx_image(
            1,
            1,
            4,
            vec![128, 0, 0, 128],
            pdfboss_jpx::ColorKind::Rgb,
            Some(3),
        );
        let meta = jpx_meta(&doc, b"<< /SMaskInData 2 >>", None);
        let (img, notes) = jpx_rgba(&meta, image).expect("decodes");
        assert!(
            notes.is_empty(),
            "a clean division is not a loss: {notes:?}"
        );
        assert_eq!(rgba_at(&img, 0, 0), [255, 0, 0, 128]);
    }

    #[test]
    fn jpx_premultiplied_color_under_zero_alpha_clamps_with_one_note() {
        let doc = test_doc();
        // Two malformed pixels (colour where alpha says none): one note.
        let image = jpx_image(
            2,
            1,
            2,
            vec![9, 0, 17, 0],
            pdfboss_jpx::ColorKind::Gray,
            Some(1),
        );
        let meta = jpx_meta(&doc, b"<< /SMaskInData 2 >>", None);
        let (img, notes) = jpx_rgba(&meta, image).expect("decodes");
        assert_eq!(rgba_at(&img, 0, 0), [0, 0, 0, 0], "clamped, fully clear");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("zero opacity"), "{notes:?}");
    }

    #[test]
    fn jpx_data_loss_warnings_surface_and_benign_notes_do_not() {
        // The split is the decoder's own `data_loss` flag, never the message
        // text: the benign note below name-drops "corrupt code-block" and the
        // loss reads like an advisory, and the classification must not care.
        let doc = test_doc();
        let mut image = jpx_image(1, 1, 1, vec![0], pdfboss_jpx::ColorKind::Gray, None);
        image.warnings = vec![
            pdfboss_jpx::JpxWarning {
                message: "2 tile(s) ship more tile-parts than their declared TNsot \
                          (violates T.800 A.4.2); tolerated for compatibility"
                    .to_string(),
                data_loss: false,
            },
            pdfboss_jpx::JpxWarning {
                message: "a benign note mentioning a corrupt code-block".to_string(),
                data_loss: false,
            },
            pdfboss_jpx::JpxWarning {
                message: "tile 3 rendered as background".to_string(),
                data_loss: true,
            },
        ];
        let meta = jpx_meta(&doc, b"<< >>", None);
        let (_, notes) = jpx_rgba(&meta, image).expect("decodes");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0], "JPXDecode: tile 3 rendered as background");
    }

    #[test]
    fn trailing_filter_skips_non_name_entries_like_decode_stream() {
        // decode_stream reads /Filter by keeping only the Name entries, so
        // `[/JPXDecode null]` passes the codestream through; the image
        // layer's reading of "trailing" must agree, or the raw bytes get
        // painted as samples.
        let doc = test_doc();
        let d = dict(b"<< /Width 1 /Height 1 /Filter [/JPXDecode null] >>");
        let meta = ImageMeta::read(&doc, &d, None);
        assert!(meta.jpx, "the trailing Name is JPXDecode");

        let d = dict(b"<< /Width 1 /Height 1 /Filter [/FlateDecode /DCTDecode null] >>");
        let meta = ImageMeta::read(&doc, &d, None);
        assert!(meta.dct, "the trailing Name is DCTDecode");

        let d = dict(b"<< /Width 1 /Height 1 /Filter [null] >>");
        let meta = ImageMeta::read(&doc, &d, None);
        assert!(!meta.jpx && !meta.dct, "no Name, no codec");
    }

    #[test]
    fn jpx_palette_index_reverses_the_normalization_exactly() {
        // The decoder's forward normalization for depths below 8 is
        // round(i * 255 / max), computed as (i*255 + max/2) / max (the
        // pdfboss-jpx contract behind DecodedImage::component_depths).
        // Prove the reversal recovers every index of every shallow depth.
        for depth in 1u8..8 {
            let max = (1u32 << depth) - 1;
            for index in 0..=max {
                let normalized = ((index * 255 + max / 2) / max) as u8;
                assert_eq!(
                    u32::from(jpx_palette_index(normalized, depth)),
                    index,
                    "depth {depth} index {index} normalized {normalized}"
                );
            }
        }
    }

    #[test]
    fn jpx_indexed_4bit_samples_recover_their_palette_indices() {
        // A 4-bit index of 8 arrives normalized to 136; the palette must
        // be read at 8 (green), not at 136 (clamped into the red tail).
        let palette: String = (0..16)
            .map(|i| if i == 8 { "00FF00" } else { "FF0000" })
            .collect();
        let cs = format!("[/Indexed /DeviceRGB 15 <{palette}>]");
        let doc = test_doc();
        let mut image = jpx_image(1, 1, 1, vec![136], pdfboss_jpx::ColorKind::Gray, None);
        image.component_depths = vec![4];
        let meta = jpx_meta(&doc, b"<< >>", Some(cs.as_bytes()));
        let (img, notes) = jpx_rgba(&meta, image).expect("decodes");
        assert!(notes.is_empty(), "an exact reversal is silent: {notes:?}");
        assert_eq!(rgba_at(&img, 0, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn jpx_indexed_deep_channel_passes_through_with_a_note() {
        // Depth > 8 right-shifted the indices; the low bits are gone, so
        // the samples pass through unchanged and the report owns up.
        let doc = test_doc();
        let mut image = jpx_image(1, 1, 1, vec![2], pdfboss_jpx::ColorKind::Gray, None);
        image.component_depths = vec![12];
        let meta = jpx_meta(
            &doc,
            b"<< >>",
            Some(b"[/Indexed /DeviceRGB 3 <FF0000 00FF00 0000FF 000000>]"),
        );
        let (img, notes) = jpx_rgba(&meta, image).expect("decodes");
        assert_eq!(rgba_at(&img, 0, 0), [0, 0, 255, 255], "index 2 as stored");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("12-bit"), "{notes:?}");
    }

    #[test]
    fn jpx_stencil_paints_fill_where_the_sample_maps_to_zero() {
        // ISO 32000-1 7.4.9: a 1-bit channel normalizes to samples of 0 or
        // 255. The default /Decode paints the 0 samples in the fill colour
        // and leaves the 1 samples untouched; [1 0] flips that.
        let doc = test_doc();
        let image = || {
            let mut image = jpx_image(2, 1, 1, vec![0, 255], pdfboss_jpx::ColorKind::Gray, None);
            image.component_depths = vec![1];
            image
        };

        let meta = jpx_meta(&doc, b"<< /ImageMask true >>", None);
        let (img, notes) = jpx_stencil(&meta, &image(), [10, 20, 30]).expect("stencils");
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(rgba_at(&img, 0, 0), [10, 20, 30, 255], "0 paints fill");
        assert_eq!(rgba_at(&img, 1, 0), [0, 0, 0, 0], "1 stays clear");

        let meta = jpx_meta(&doc, b"<< /ImageMask true /Decode [1 0] >>", None);
        let (img, _) = jpx_stencil(&meta, &image(), [10, 20, 30]).expect("stencils");
        assert_eq!(rgba_at(&img, 0, 0), [0, 0, 0, 0], "inverted: 0 clear");
        assert_eq!(rgba_at(&img, 1, 0), [10, 20, 30, 255], "inverted: 1 paints");
    }

    #[test]
    fn jpx_stencil_rejects_a_multichannel_codestream() {
        let doc = test_doc();
        let image = jpx_image(1, 1, 3, vec![1, 2, 3], pdfboss_jpx::ColorKind::Rgb, None);
        let meta = jpx_meta(&doc, b"<< /ImageMask true >>", None);
        let reason = match jpx_stencil(&meta, &image, [0; 3]) {
            Err(reason) => reason,
            Ok(..) => panic!("three channels are not a stencil"),
        };
        assert!(
            reason.contains("3 channels") && reason.contains("ImageMask"),
            "{reason}"
        );
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

    fn quad_image() -> Rgba<'static> {
        // Row 0: red, green; row 1: blue, white.
        Rgba {
            width: 2,
            height: 2,
            pixels: Pixels::Quads(vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 255, 255, 255, 255,
            ]),
            truncated: false,
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
            blend: BlendMode::Normal,
            smask: None,
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
            blend: BlendMode::Normal,
            smask: None,
        };
        draw_rgba(&mut pix, &quad_image(), &p);
        assert_eq!(pix_at(&pix, 1, 1), [255, 255, 255, 255], "outside image");
        let [r, g, b, _] = pix_at(&pix, 5, 1);
        assert_eq!(b, 255, "blue keeps its own channel");
        assert!((127..=129).contains(&r), "50% blend r {r}");
        assert!((127..=129).contains(&g), "50% blend g {g}");
        assert_eq!(pix_at(&pix, 7, 1), [255, 255, 255, 255], "clipped column");
    }

    /// [`draw_rgba`] short-circuits a fully opaque source to a copy instead of
    /// calling [`composite_over`]. That is only sound if the general formula
    /// returns the very same bytes at alpha 1, whatever is underneath, so this
    /// checks it does — over a spread of destination colors and alphas.
    #[test]
    fn an_opaque_source_composites_to_a_plain_copy() {
        for &under in &[
            [0, 0, 0, 0],
            [255, 255, 255, 255],
            [17, 200, 3, 128],
            [9, 9, 9, 1],
        ] {
            for &rgb in &[[0, 0, 0], [255, 255, 255], [12, 34, 56]] {
                let mut dst = under;
                composite_over(&mut dst, rgb, 1.0);
                assert_eq!(
                    dst,
                    [rgb[0], rgb[1], rgb[2], 255],
                    "opaque {rgb:?} over {under:?}"
                );
            }
        }
    }

    #[test]
    fn degenerate_ctm_draws_nothing() {
        let mut pix = Pixmap::new(4, 4);
        let p = DrawParams {
            ctm: Matrix::scale(0.0, 0.0),
            alpha: 1.0,
            fill_rgb: [0; 3],
            clip: None,
            blend: BlendMode::Normal,
            smask: None,
        };
        draw_rgba(&mut pix, &quad_image(), &p);
        assert!(pix.data.iter().all(|&b| b == 0), "pixmap untouched");
    }
}
