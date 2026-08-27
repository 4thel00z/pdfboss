//! Markdown to PDF composition for pdfboss: parses CommonMark+GFM source
//! into a block tree for later layout.

pub mod block;
pub mod report;

pub use report::Report;
