//! Markdown to PDF composition for pdfboss: takes CommonMark+GFM source and
//! a CSS theme in, and produces a [`pdfboss_write::Pdf`] plus a
//! replace-and-report [`Report`] of anything sanitized along the way.
//! Given the same markdown and options, [`to_pdf`] always emits the same
//! bytes: no clock, no randomness, no environment dependence.

pub mod block;
mod emit;
mod layout;
pub mod report;
mod table;
mod wrap;

use std::path::PathBuf;

pub use block::{Block, CellAlign, ListItem, Run};
pub use pdfboss_style::{StyleError, Theme};
pub use pdfboss_write::{PageSize, Pdf};
pub use report::Report;

/// Errors raised while laying out or writing a Markdown-composed document.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An image referenced by the document could not be loaded or decoded.
    #[error("{path}: {message}")]
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

/// Composition options for [`to_pdf`].
pub struct Options {
    /// The CSS theme cascading over every element.
    pub theme: Theme,
    /// The page size every page is laid out and emitted at.
    pub page_size: PageSize,
    /// The directory local image paths resolve against.
    pub base_dir: PathBuf,
}

impl Default for Options {
    /// The built-in default theme, A4 pages, and the current directory as
    /// the image base.
    fn default() -> Options {
        Options {
            theme: Theme::default_theme(),
            page_size: PageSize::A4,
            base_dir: PathBuf::from("."),
        }
    }
}

/// Parses `markdown`, lays it out under `options`, and emits a
/// [`pdfboss_write::Pdf`] ready to serialize, alongside a [`Report`] of
/// unencodable characters replaced and raw HTML fragments skipped.
pub fn to_pdf(markdown: &str, options: &Options) -> Result<(Pdf, Report), Error> {
    let (blocks, skipped_html) = block::parse_blocks(markdown);
    let mut report = Report {
        skipped_html,
        ..Report::default()
    };
    let laid = layout::layout(
        &blocks,
        &options.theme,
        options.page_size,
        &options.base_dir,
        &mut report,
    )?;
    let pages = emit::emit(laid, options.page_size)?;
    Ok((
        Pdf {
            pages,
            ..Pdf::default()
        },
        report,
    ))
}
