//! PDF creation for pdfboss: the write-side twin of `pdfboss-core`.
//!
//! Three altitudes, each complete on its own and lowering into the one
//! beneath:
//!
//! - **document** — [`Pdf`] and [`Page`]: plain structs whose fields are
//!   the composition, saved with [`Pdf::save`].
//! - **canvas** — [`Canvas`]: an imperative painter accumulating
//!   `pdfboss_core::content::Op`, the same IR the reader parses, so every
//!   generated content stream round-trips through `parse_content`.
//! - **cos** — [`Writer`]: numbered objects in, finished file bytes out,
//!   with stream compression, object streams, both cross-reference styles
//!   and a deterministic `/ID`.
//!
//! Determinism is a feature: the same input produces byte-identical
//! output. The crate never reads clocks or randomness — dates appear only
//! when callers supply them.

pub mod canvas;
pub mod color;
pub mod content;
pub mod element;
pub mod error;
pub mod font;
pub mod image;
pub mod importer;
pub mod pdf;
pub mod ser;
pub mod sink;
pub mod update;
pub mod writer;
mod xmp;

pub use canvas::{BlendMode, Canvas, CanvasParts, GroupHandle, ImageHandle, LineCap, LineJoin};
pub use color::Color;
pub use content::serialize_ops;
pub use element::{Content, Draw, Image, Link, Paragraph, ParagraphAlign, Text};
pub use error::{Error, Result};
pub use font::Standard14;
pub use image::ImageData;
pub use importer::Importer;
pub use pdf::{
    Attachment, Bookmark, Date, LabelStyle, LinkAnnotation, LinkTarget, Metadata, Outline, Page,
    PageLabel, PageLayout, PageMode, PageSize, Pdf, Viewer,
};
pub use sink::{AsyncByteSink, Immediate};
pub use update::{
    set_metadata_with, start_offset, watermark, watermark_with, Overlay, OverlayBase, Update,
};
pub use writer::{WriteOptions, Writer, XrefStyle};
