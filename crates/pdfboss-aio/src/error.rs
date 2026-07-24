//! Error type for pdfboss-aio: wraps core parse errors and transport
//! failures, with dedicated variants for range-refusing HTTP servers and
//! short reads. Messages are prefixed by layer ("parse:", "i/o:", "http:")
//! so downstream consumers can present them uniformly.

/// Convenience alias used throughout pdfboss-aio.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors surfaced by pdfboss-aio.
///
/// Every fetch failure carries the offset/range it was fetching, for
/// diagnosability; parse errors wrap the core error unchanged.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A parse-layer error from the sync core machinery.
    #[error("parse: {0}")]
    Core(#[from] pdfboss_core::Error),
    /// A transport-layer I/O error.
    #[error("i/o: {0}")]
    Io(std::io::Error),
    /// An HTTP transport error (connection, status, malformed response).
    #[cfg(feature = "http")]
    #[error("http: {msg}")]
    Http { status: Option<u16>, msg: String },
    /// The server ignored `Range` requests (answered 200 with the full
    /// body instead of 206), so range-fetching cannot work.
    #[error("server ignored Range requests")]
    RangeUnsupported,
    /// A read stopped short of the requested range while more bytes were
    /// expected (the source is shorter than its declared length).
    #[error("truncated read at offset {offset}: wanted {wanted} bytes, got {got}")]
    TruncatedRead {
        offset: u64,
        wanted: usize,
        got: usize,
    },
}

impl From<std::io::Error> for Error {
    fn from(inner: std::io::Error) -> Error {
        #[cfg(feature = "http")]
        if let Some(marker) = inner
            .get_ref()
            .and_then(|source| source.downcast_ref::<TransportMarker>())
        {
            return match marker {
                TransportMarker::RangeUnsupported => Error::RangeUnsupported,
                TransportMarker::Http { status, msg } => Error::Http {
                    status: *status,
                    msg: msg.clone(),
                },
            };
        }
        Error::Io(inner)
    }
}

/// Marker payload smuggled through `std::io::Error` by backends whose
/// trait methods can only return `io::Result`; recovered by
/// [`From<std::io::Error>`] above. Only the HTTP backend produces these.
#[cfg(feature = "http")]
#[derive(Debug)]
#[allow(dead_code)] // TODO: remove once HTTP backend task constructs these variants
pub(crate) enum TransportMarker {
    RangeUnsupported,
    Http { status: Option<u16>, msg: String },
}

#[cfg(feature = "http")]
impl std::fmt::Display for TransportMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportMarker::RangeUnsupported => write!(f, "server ignored Range requests"),
            TransportMarker::Http { status, msg } => write!(f, "http {status:?}: {msg}"),
        }
    }
}

#[cfg(feature = "http")]
impl std::error::Error for TransportMarker {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_core_and_io_errors_with_layer_prefixes() {
        let core = Error::from(pdfboss_core::Error::InvalidXref);
        assert!(matches!(
            core,
            Error::Core(pdfboss_core::Error::InvalidXref)
        ));
        assert_eq!(
            core.to_string(),
            "parse: invalid or unrecoverable cross-reference data"
        );
        let io = Error::from(std::io::Error::other("boom"));
        assert!(matches!(io, Error::Io(_)));
        assert_eq!(io.to_string(), "i/o: boom");
    }

    #[test]
    fn transport_variants_render_their_context() {
        let err = Error::TruncatedRead {
            offset: 512,
            wanted: 100,
            got: 3,
        };
        assert_eq!(
            err.to_string(),
            "truncated read at offset 512: wanted 100 bytes, got 3"
        );
        assert_eq!(
            Error::RangeUnsupported.to_string(),
            "server ignored Range requests"
        );
    }
}
