//! Shared test support: a backend wrapper that logs every read.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use pdfboss_aio::{Backend, BoxFuture};

/// Wraps a backend, logging every `read_at`: total bytes returned and
/// call count, observable through the paired [`ReadLog`].
pub struct RecordingBackend<B> {
    inner: B,
    bytes: Arc<AtomicU64>,
    calls: Arc<AtomicUsize>,
}

impl<B: Backend> RecordingBackend<B> {
    pub fn new(inner: B) -> (RecordingBackend<B>, ReadLog) {
        let bytes = Arc::new(AtomicU64::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let log = ReadLog {
            bytes: Arc::clone(&bytes),
            calls: Arc::clone(&calls),
        };
        (
            RecordingBackend {
                inner,
                bytes,
                calls,
            },
            log,
        )
    }
}

/// Shared counters observed by tests after the document is consumed.
pub struct ReadLog {
    bytes: Arc<AtomicU64>,
    calls: Arc<AtomicUsize>,
}

impl ReadLog {
    pub fn total_bytes(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }

    pub fn read_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl<B: Backend> Backend for RecordingBackend<B> {
    fn len(&self) -> BoxFuture<'_, std::io::Result<u64>> {
        self.inner.len()
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> BoxFuture<'a, std::io::Result<usize>> {
        Box::pin(async move {
            let count = self.inner.read_at(offset, buf).await?;
            self.bytes.fetch_add(count as u64, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(count)
        })
    }
}
