//! Error surface for the decoder.
//!
//! Contract (see the crate docs): header-level problems are hard errors;
//! once the first packet of the image decodes, corruption degrades to
//! warnings on the [`crate::DecodedImage`] instead — whichever tile it
//! strikes.

use std::fmt;

/// Decoder error.
///
/// `#[non_exhaustive]`: later stages may grow variants without a breaking
/// change; downstream matches must keep a wildcard arm.
#[non_exhaustive]
#[derive(Debug)]
pub enum JpxError {
    /// The input is neither a JP2-family file (ITU-T T.800 I.5.1 signature
    /// box) nor a raw codestream (A.4.1 SOC marker followed by an A.5.1 SIZ
    /// marker).
    NotJpeg2000,
    /// A structurally invalid box (Annex I) or marker segment (Annex A)
    /// before the first packet of the image decoded. The string names the
    /// offending structure and what was wrong with it.
    Malformed(String),
    /// A `DecodeLimits` bound was exceeded. `what` names the bound.
    LimitExceeded {
        /// Which limit tripped (e.g. `"max_pixels"`).
        what: &'static str,
        /// The value derived from the input headers.
        actual: u64,
        /// The configured bound.
        limit: u64,
    },
    /// The codestream uses a feature outside the supported profile
    /// (T.800 features this decoder lists as out of scope). The string
    /// names the feature.
    Unsupported(&'static str),
}

impl fmt::Display for JpxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JpxError::NotJpeg2000 => {
                write!(f, "not a JPEG 2000 file or codestream")
            }
            JpxError::Malformed(detail) => {
                write!(f, "malformed JPEG 2000 data: {detail}")
            }
            JpxError::LimitExceeded {
                what,
                actual,
                limit,
            } => {
                write!(f, "decode limit exceeded: {what} = {actual} > {limit}")
            }
            JpxError::Unsupported(what) => {
                write!(f, "unsupported JPEG 2000 feature: {what}")
            }
        }
    }
}

impl std::error::Error for JpxError {}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, JpxError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_the_tripped_limit() {
        let err = JpxError::LimitExceeded {
            what: "max_pixels",
            actual: 200,
            limit: 100,
        };
        assert_eq!(
            err.to_string(),
            "decode limit exceeded: max_pixels = 200 > 100"
        );
    }
}
