//! Error type for PDF creation. Writing is strict where reading is lenient:
//! a writer that silently drops or corrupts content is worse than one that
//! refuses, so every lossy situation is an error, never a skip.

use pdfboss_core::ObjRef;

/// Alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Any error raised while building or serializing a PDF.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying I/O failure while saving.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A reserved object was filled twice, or `fill` targeted a
    /// never-reserved reference.
    #[error("object {} {} already has a body", .0.num, .0.gen)]
    AlreadyFilled(ObjRef),
    /// `finish` found a reserved object that was never filled.
    #[error("object {} {} was reserved but never filled", .0.num, .0.gen)]
    Unfilled(ObjRef),
    /// A `Stream` object appeared nested inside another object; streams are
    /// only legal as indirect objects (ISO 32000 §7.3.8).
    #[error("stream objects must be indirect, not nested")]
    NestedStream,
    /// A character has no code in the target font's encoding.
    #[error("character {ch:?} is not encodable in {font}")]
    Unencodable {
        /// The character that failed to encode.
        ch: char,
        /// Base font name of the font that rejected it.
        font: &'static str,
    },
    /// Image bytes could not be understood or are inconsistent.
    #[error("invalid image: {0}")]
    Image(String),
    /// Anything else.
    #[error("{0}")]
    Other(String),
}
