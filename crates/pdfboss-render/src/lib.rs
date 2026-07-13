//! Page rasterization for pdfboss: paths, fills, strokes, clipping, color
//! spaces, images and embedded-TrueType glyph outlines, rendered to an RGBA8
//! pixmap and encodable as PNG.

// The rasterizer modules are consumed by the content-stream executor; the
// `dead_code` allowances below disappear once it is wired up.
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
mod truetype;

use std::path::Path;

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

/// Renders a page at `scale` onto a white background. The pixel size is
/// `ceil(crop_w * scale) x ceil(crop_h * scale)` (after `/Rotate`), and the
/// base transform maps the crop box to device space with a y-flip and the
/// page rotation applied.
pub fn render_page(doc: &Document, page: &Page, scale: f32) -> Result<Pixmap> {
    executor::render_page(doc, page, scale)
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
