//! Page rasterization for pdfboss: paths, fills, strokes, clipping, color
//! spaces, images and embedded-TrueType glyph outlines, rendered to an RGBA8
//! pixmap and encodable as PNG.

// The rasterizer modules are consumed by the content-stream executor; the
// `dead_code` allowances below disappear once it is wired up.
mod cff;
#[allow(dead_code)]
mod color;
mod executor;
mod glyph;
mod image;
#[allow(dead_code)]
mod path;
#[allow(dead_code)]
mod raster;
#[allow(dead_code)]
mod stroke;
#[allow(dead_code)]
mod substitute;
mod truetype;
mod type1;
mod type3;

use std::path::{Path, PathBuf};

use pdfboss_core::{Document, Error, Page, Result};

/// An RGBA8 raster image with straight (non-premultiplied) alpha, row-major
/// from the top-left.
#[derive(Debug, Clone, PartialEq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    /// Pixel data, `width * height * 4` bytes (RGBA per pixel).
    pub data: Vec<u8>,
}

impl Pixmap {
    /// Creates a fully transparent pixmap.
    pub fn new(w: u32, h: u32) -> Pixmap {
        Pixmap {
            width: w,
            height: h,
            data: vec![0; w as usize * h as usize * 4],
        }
    }

    /// Fills every pixel with `rgba`.
    pub fn fill(&mut self, rgba: [u8; 4]) {
        for px in self.data.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
    }

    /// Encodes the pixmap as a PNG image.
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        fn err(e: png::EncodingError) -> Error {
            Error::Other(format!("png encode: {e}"))
        }
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(err)?;
        writer.write_image_data(&self.data).map_err(err)?;
        writer.finish().map_err(err)?;
        Ok(out)
    }

    /// Encodes the pixmap as PNG and writes it to `path`.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.encode_png()?)?;
        Ok(())
    }
}

/// How aggressively the rasterizer turns text into filled outlines. Each tier is
/// a strict superset of the previous one; the difference is only observable once
/// the corresponding glyph loaders exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlyphPainting {
    /// Only embedded TrueType (`glyf`) outlines — the cheapest tier.
    EmbeddedTrueTypeOnly,
    /// Every embedded program: TrueType, CFF, Type1 and Type3. No bundled assets.
    #[default]
    AllEmbedded,
    /// Also substitute bundled or caller-provided faces for non-embedded fonts.
    Full,
}

impl GlyphPainting {
    /// Whether this tier paints every embedded program (CFF, Type1, Type3),
    /// not just embedded TrueType.
    pub fn paints_all_embedded(self) -> bool {
        !matches!(self, GlyphPainting::EmbeddedTrueTypeOnly)
    }
}

/// Where non-embedded glyph substitution (the `Full` [`GlyphPainting`] tier)
/// draws replacement faces from. The default, `None`, substitutes nothing --
/// `Full` behaves exactly like `AllEmbedded` until a caller opts in.
#[derive(Clone, Debug, Default)]
pub enum SubstituteSource {
    /// No substitution: non-embedded fonts stay unpainted.
    #[default]
    None,
    /// Compiled-in faces (wired up in a later plan, gated by a
    /// `substitute-fonts`-style feature).
    Builtin,
    /// Faces read from a directory at render time (e.g. an installed
    /// `pdfboss-fonts` package), one file per style -- see
    /// `substitute::face_filename`.
    Dir(PathBuf),
}

/// Options controlling a single page render.
#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    /// Which font programs the rasterizer will paint.
    pub glyph_painting: GlyphPainting,
    /// Where `Full`-tier substitution draws replacement faces from. Ignored
    /// at every other tier.
    pub substitutes: SubstituteSource,
}

/// Whether this binary was built with the `substitute-fonts` feature, i.e.
/// whether `SubstituteSource::Builtin` has compiled-in faces to hand out.
/// Callers (e.g. the CLI) use this to give an actionable message when `Full`
/// is requested with no `--font-dir` and no compiled-in set, rather than
/// silently rendering as if `Full` had never been asked for.
pub fn builtin_fonts_available() -> bool {
    cfg!(feature = "substitute-fonts")
}

/// Renders a page at `scale` onto a white background. The pixel size is
/// `ceil(crop_w * scale) x ceil(crop_h * scale)` (after `/Rotate`), and the
/// base transform maps the crop box to device space with a y-flip and the
/// page rotation applied.
pub fn render_page(doc: &Document, page: &Page, scale: f32) -> Result<Pixmap> {
    render_page_with_options(doc, page, scale, &RenderOptions::default())
}

/// Renders a page like [`render_page`], honoring `opts` (currently the glyph
/// painting tier). See [`render_page`] for the geometry contract.
pub fn render_page_with_options(
    doc: &Document,
    page: &Page,
    scale: f32,
    opts: &RenderOptions,
) -> Result<Pixmap> {
    executor::render_page_with_options(doc, page, scale, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pixmap_is_transparent() {
        let pix = Pixmap::new(3, 2);
        assert_eq!(pix.width, 3);
        assert_eq!(pix.height, 2);
        assert_eq!(pix.data.len(), 24);
        assert!(pix.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn fill_sets_every_pixel() {
        let mut pix = Pixmap::new(2, 2);
        pix.fill([1, 2, 3, 4]);
        assert_eq!(pix.data, [1, 2, 3, 4].repeat(4));
    }

    #[test]
    fn png_round_trip_preserves_pixels() {
        let mut pix = Pixmap::new(3, 2);
        for (i, b) in pix.data.iter_mut().enumerate() {
            *b = (i * 11 % 256) as u8;
        }
        let bytes = pix.encode_png().expect("encode");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

        let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        let mut reader = decoder.read_info().expect("read_info");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut buf).expect("frame");
        assert_eq!(info.width, 3);
        assert_eq!(info.height, 2);
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        assert_eq!(&buf[..info.buffer_size()], &pix.data[..]);
    }

    #[test]
    fn save_png_writes_decodable_file() {
        let mut pix = Pixmap::new(4, 4);
        pix.fill([10, 20, 30, 255]);
        let dir = std::env::temp_dir().join("pdfboss-render-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pix.png");
        pix.save_png(&path).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::remove_file(&path).ok();
    }
}
