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
    /// A transport-layer failure from an asynchronous byte source — an I/O
    /// error, an HTTP failure, a refused range request, a short read.
    ///
    /// Carried as a rendered message so this crate stays free of transport
    /// types. Produced when a source whose own error type is richer (the
    /// asynchronous document's) crosses a shared-algorithm boundary that
    /// speaks this `Result`; the adapter unwraps its parse-layer errors back
    /// to the original variant and renders only the genuinely transport-level
    /// remainder into this one.
    #[error("transport: {0}")]
    Transport(String),
    #[error("{0}")]
    Other(String),
}
