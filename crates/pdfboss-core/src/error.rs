//! Error and result types shared across all pdfboss crates.

/// Convenience alias used throughout pdfboss.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors surfaced by pdfboss.
///
/// Parsing is lenient by design; hard errors are reserved for unreadable
/// cross-reference data (after recovery), encryption, and I/O.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("syntax error at byte {offset}: {msg}")]
    Syntax { offset: usize, msg: String },
    #[error("invalid or unrecoverable cross-reference data")]
    InvalidXref,
    #[error("object {0} {1} not found")]
    ObjectNotFound(u32, u16),
    #[error("missing required key /{0}")]
    MissingKey(&'static str),
    #[error("type mismatch: expected {expected}, found {found}")]
    TypeMismatch {
        expected: &'static str,
        found: &'static str,
    },
    #[error("unsupported filter /{0}")]
    UnsupportedFilter(String),
    #[error("stream decode failed: {0}")]
    Decode(String),
    #[error("encrypted documents are not supported")]
    Encrypted,
    #[error("page index {0} out of bounds ({1} pages)")]
    PageNotFound(usize, usize),
    #[error("circular reference involving object {0}")]
    CircularReference(u32),
    #[error("{0}")]
    Other(String),
}
