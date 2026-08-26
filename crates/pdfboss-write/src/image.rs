//! Image import for document creation. JPEG passes through untouched as
//! `/DCTDecode` (dimensions sniffed from its SOF marker); PNG is decoded
//! to a raster with its alpha split into an `/SMask`; raw rasters cover
//! generated content. No encode-side JPX/JBIG2/CCITT — by design.

use pdfboss_core::ObjRef;

use crate::error::Result;
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
    Jpeg {
        data: Vec<u8>,
        gray: bool,
    },
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
        todo!("decode png ({} bytes)", bytes.len())
    }

    /// Imports a baseline or progressive JPEG by passthrough. Dimensions
    /// and component count are read from the SOF marker; grayscale and
    /// three-component (YCbCr/RGB) images are supported, anything else is
    /// an error.
    pub fn jpeg(bytes: &[u8]) -> Result<ImageData> {
        todo!("sniff jpeg ({} bytes)", bytes.len())
    }

    /// Wraps an 8-bit grayscale raster; `data` is `width * height` bytes.
    pub fn gray8(width: u32, height: u32, data: Vec<u8>) -> Result<ImageData> {
        let unused = (width, height, data);
        todo!("gray8 raster: {unused:?}")
    }

    /// Wraps an 8-bit RGB raster; `data` is `width * height * 3` bytes.
    pub fn rgb8(width: u32, height: u32, data: Vec<u8>) -> Result<ImageData> {
        let unused = (width, height, data);
        todo!("rgb8 raster: {unused:?}")
    }

    /// Wraps a 1-bit raster; rows are packed MSB-first, each row padded to
    /// a whole byte. A set bit is black (sample 0).
    pub fn mono(width: u32, height: u32, data: Vec<u8>) -> Result<ImageData> {
        let unused = (width, height, data);
        todo!("mono raster: {unused:?}")
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
    /// XObject(s), returning the image object's reference.
    pub(crate) fn build_xobject(&self, w: &mut Writer) -> ObjRef {
        let unused = (self, w);
        todo!("build xobject: {unused:?}")
    }
}
