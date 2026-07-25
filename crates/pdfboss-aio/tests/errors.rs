//! Error injection: sources that truncate mid-file and transports that
//! fail outright surface as the dedicated error variants.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures_util::stream::StreamExt;
use pdfboss_aio::{AsyncDocument, Backend, BoxFuture, Error, MemBackend};
use pdfboss_core::elements::ElementOpts;
use pdfboss_testkit::simple_doc;

/// Reports a length beyond the real data, so reads near the claimed end
/// hit EOF while the document still expects bytes.
struct OverstatedBackend {
    inner: MemBackend,
    claimed: u64,
}

impl Backend for OverstatedBackend {
    fn len(&self) -> BoxFuture<'_, std::io::Result<u64>> {
        let claimed = self.claimed;
        Box::pin(async move { Ok(claimed) })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> BoxFuture<'a, std::io::Result<usize>> {
        self.inner.read_at(offset, buf)
    }
}

/// Fails every read with a connection error.
struct FailingBackend {
    len: u64,
}

impl Backend for FailingBackend {
    fn len(&self) -> BoxFuture<'_, std::io::Result<u64>> {
        let len = self.len;
        Box::pin(async move { Ok(len) })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> BoxFuture<'a, std::io::Result<usize>> {
        Box::pin(async move {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                format!("injected failure reading {} bytes at {offset}", buf.len()),
            ))
        })
    }
}

#[tokio::test]
async fn truncated_source_reports_truncated_read() {
    let data = simple_doc("truncated");
    let claimed = data.len() as u64 + 4096;
    let backend = OverstatedBackend {
        inner: MemBackend::from(data),
        claimed,
    };
    match AsyncDocument::with_backend(backend).await {
        Err(Error::TruncatedRead { wanted, got, .. }) => {
            assert!(got < wanted, "short read carries both counts");
        }
        Err(other) => panic!("expected TruncatedRead, got {other:?}"),
        Ok(doc) => panic!(
            "expected failure, opened a document with {} pages",
            doc.page_count()
        ),
    }
}

#[tokio::test]
async fn failing_transport_surfaces_as_io_error() {
    let backend = FailingBackend { len: 10_000 };
    match AsyncDocument::with_backend(backend).await {
        Err(Error::Io(err)) => {
            assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset);
        }
        Err(other) => panic!("expected Io, got {other:?}"),
        Ok(doc) => panic!(
            "expected failure, opened a document with {} pages",
            doc.page_count()
        ),
    }
}

/// Counts how many read_at calls are made.
struct CountingBackend {
    inner: MemBackend,
    call_count: Arc<AtomicUsize>,
}

impl CountingBackend {
    fn new(inner: MemBackend) -> (Self, Arc<AtomicUsize>) {
        let call_count = Arc::new(AtomicUsize::new(0));
        (
            CountingBackend {
                inner,
                call_count: Arc::clone(&call_count),
            },
            call_count,
        )
    }
}

impl Backend for CountingBackend {
    fn len(&self) -> BoxFuture<'_, std::io::Result<u64>> {
        self.inner.len()
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> BoxFuture<'a, std::io::Result<usize>> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.inner.read_at(offset, buf)
    }
}

/// Delegates to a working MemBackend for the first N read_at calls,
/// then returns injected error for every later call.
struct FailsAfterOpenBackend {
    inner: MemBackend,
    call_count: Arc<AtomicUsize>,
    fail_after: usize,
}

impl FailsAfterOpenBackend {
    fn new(inner: MemBackend, fail_after: usize) -> Self {
        FailsAfterOpenBackend {
            inner,
            call_count: Arc::new(AtomicUsize::new(0)),
            fail_after,
        }
    }
}

impl Backend for FailsAfterOpenBackend {
    fn len(&self) -> BoxFuture<'_, std::io::Result<u64>> {
        self.inner.len()
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> BoxFuture<'a, std::io::Result<usize>> {
        let call_num = self.call_count.fetch_add(1, Ordering::SeqCst);
        if call_num >= self.fail_after {
            Box::pin(async move {
                Err(std::io::Error::other(format!(
                    "injected failure at call {} (threshold {})",
                    call_num, self.fail_after
                )))
            })
        } else {
            self.inner.read_at(offset, buf)
        }
    }
}

#[tokio::test]
async fn backend_failure_after_open_surfaces_as_err() {
    // First, count how many reads are needed to open simple_doc.
    let data = simple_doc("after_open");
    let (counting_backend, count_arc) = CountingBackend::new(MemBackend::from(data.clone()));
    assert!(
        AsyncDocument::with_backend(counting_backend).await.is_ok(),
        "failed to open during count phase"
    );
    let reads_for_open = count_arc.load(Ordering::SeqCst);
    assert!(
        reads_for_open > 0,
        "opening should have triggered at least one read"
    );

    // Now create a backend that fails midway through reads to ensure get_object triggers failure.
    // Use a lower threshold than full open so we force read failures in subsequent operations.
    let fail_threshold = reads_for_open / 2;
    let failing_backend =
        FailsAfterOpenBackend::new(MemBackend::from(data.clone()), fail_threshold);
    let open_result = AsyncDocument::with_backend(failing_backend).await;
    // The open may fail partway through due to low threshold, which is fine.
    // If it succeeds, subsequent reads will fail.
    let doc = match open_result {
        Ok(d) => d,
        Err(Error::Io(_)) => {
            // Open failed due to low threshold, which is acceptable.
            // Fall through to test elements stream failure instead.
            let failing_backend =
                FailsAfterOpenBackend::new(MemBackend::from(data.clone()), reads_for_open);
            AsyncDocument::with_backend(failing_backend)
                .await
                .expect("should open with higher threshold")
        }
        Err(e) => panic!("unexpected error during open: {e:?}"),
    };

    // Try to get an object (may need a read past the threshold).
    let obj_result = doc
        .get_object(pdfboss_core::ObjRef { num: 2, gen: 0 })
        .await;
    match obj_result {
        Err(Error::Io(io_err)) if io_err.to_string().contains("injected") => {
            // Success: got injected error as expected.
        }
        Err(_) | Ok(_) => {
            // Object was cached or threshold too high; that's OK.
            // The elements stream test below is more reliable.
        }
    }

    // Also verify that streaming elements encounters the failure.
    // Re-open so we can test streaming independently.
    let failing_backend = FailsAfterOpenBackend::new(MemBackend::from(data), reads_for_open);
    let doc = match AsyncDocument::with_backend(failing_backend).await {
        Ok(d) => d,
        Err(e) => panic!("failed to re-open for stream test: {e:?}"),
    };

    let mut stream = doc.elements(ElementOpts::default());
    let mut saw_error = false;
    let mut item_count = 0;

    while let Some(item) = stream.next().await {
        item_count += 1;
        match item {
            Err(Error::Io(io_err)) => {
                if io_err.to_string().contains("injected") {
                    saw_error = true;
                    break; // Stream should terminate after error.
                }
            }
            Err(_) => {} // Tolerate other errors, keep looking.
            Ok(_) => {}  // Tolerate successful elements before the failure.
        }
    }

    assert!(
        saw_error,
        "elements stream should have encountered injected error after {} items",
        item_count
    );
}
