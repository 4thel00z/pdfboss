//! Async, range-fetching PDF access for pdfboss: open huge files without
//! loading them, hold many documents concurrently, and read remote PDFs
//! over HTTP range requests. Built sans-I/O style on the synchronous
//! pdfboss-core machinery: bytes are fetched in small windows and handed
//! to the existing sync lexer, parser and filters. The whole file is
//! never read.

pub mod backend;
pub mod cache;
pub mod document;
pub mod error;
#[cfg(feature = "write")]
pub mod sink;
pub mod stream;

#[cfg(feature = "http")]
pub use backend::HttpBackend;
pub use backend::{Backend, BoxFuture, FileBackend, MemBackend};
pub use cache::CachedBackend;
pub use document::AsyncDocument;
pub use error::{Error, Result};
#[cfg(feature = "write")]
pub use sink::TokioSink;
pub use stream::ElementStream;
