//! Markdown to PDF composition for pdfboss: parses CommonMark+GFM source
//! into a block tree for later layout.

pub mod block;
mod layout;
pub mod report;
mod wrap;

pub use report::Report;

/// Errors raised while laying out or writing a Markdown-composed document.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An image referenced by the document could not be loaded or decoded.
    #[error("image {path:?}: {message}")]
    Image {
        /// The image path or URI as it appeared in the document.
        path: String,
        /// A description of why the image could not be used.
        message: String,
    },
    /// A lower-level PDF-writing failure.
    #[error(transparent)]
    Write(#[from] pdfboss_write::Error),
}
