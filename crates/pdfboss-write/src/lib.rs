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
//! output, with one exception. The crate never reads clocks. Randomness is
//! read only when encrypting: `encrypt_document` and `Writer::new_encrypted`
//! draw key material and IVs from the operating system's random source by
//! default (a caller can supply its own deterministic source through
//! `Encryptor::aes256_with_rng`), so two encrypted runs need not match
//! byte-for-byte. Otherwise, dates appear only when callers supply them.

pub mod assemble;
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

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use assemble::encrypt_document;
pub use assemble::{
    decrypt_document, merge_documents, rewrite_document, rewrite_with_metadata, rotate_rewrite,
    split_document,
};
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
    rotate_pages, set_metadata_with, start_offset, watermark, watermark_under,
    watermark_under_with, watermark_with, Overlay, OverlayBase, Update,
};
pub use writer::{WriteOptions, Writer, XrefStyle};
