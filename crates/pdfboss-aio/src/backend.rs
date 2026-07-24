//! Random-access byte sources: in-memory bytes, positioned file reads on a
//! blocking thread, and (behind the `http` feature) remote HTTP range
//! requests. The trait is object-safe — futures are boxed — so documents
//! can hold `Arc<dyn Backend>`.

use std::io;

use bytes::Bytes;
pub use futures_util::future::BoxFuture;

/// Random-access byte source. Object-safe: futures are boxed.
#[allow(clippy::len_without_is_empty)]
pub trait Backend: Send + Sync + 'static {
    /// Total length of the underlying byte source.
    fn len(&self) -> BoxFuture<'_, io::Result<u64>>;

    /// Reads up to `buf.len()` bytes at `offset` into `buf`, returning the
    /// number of bytes read. Implementations may only return a short count
    /// at end of input; anywhere else they must fill the buffer.
    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8]) -> BoxFuture<'a, io::Result<usize>>;
}

/// A byte source fully resident in memory. Used directly (uncached) by
/// [`crate::document::AsyncDocument::from_bytes`].
pub struct MemBackend(Bytes);

impl From<Vec<u8>> for MemBackend {
    fn from(data: Vec<u8>) -> MemBackend {
        MemBackend(Bytes::from(data))
    }
}

impl From<Bytes> for MemBackend {
    fn from(data: Bytes) -> MemBackend {
        MemBackend(data)
    }
}

impl Backend for MemBackend {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        let total = self.0.len() as u64;
        Box::pin(async move { Ok(total) })
    }

    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8]) -> BoxFuture<'a, io::Result<usize>> {
        Box::pin(async move {
            let data = &self.0;
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(data.len());
            let count = buf.len().min(data.len() - start);
            buf[..count].copy_from_slice(&data[start..start + count]);
            Ok(count)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Backend, MemBackend};

    #[tokio::test]
    async fn mem_backend_reads_and_reports_length() {
        let backend = MemBackend::from(b"hello world".to_vec());
        assert_eq!(backend.len().await.unwrap(), 11);
        let mut buf = [0u8; 5];
        assert_eq!(backend.read_at(6, &mut buf).await.unwrap(), 5);
        assert_eq!(&buf, b"world");
    }

    #[tokio::test]
    async fn mem_backend_short_reads_only_at_eof() {
        let backend = MemBackend::from(bytes::Bytes::from_static(b"abcdef"));
        let mut buf = [0u8; 10];
        assert_eq!(backend.read_at(4, &mut buf).await.unwrap(), 2);
        assert_eq!(&buf[..2], b"ef");
        assert_eq!(backend.read_at(6, &mut buf).await.unwrap(), 0);
        assert_eq!(backend.read_at(999, &mut buf).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn backend_is_object_safe() {
        let boxed: std::sync::Arc<dyn Backend> =
            std::sync::Arc::new(MemBackend::from(b"xyz".to_vec()));
        assert_eq!(boxed.len().await.unwrap(), 3);
    }
}
