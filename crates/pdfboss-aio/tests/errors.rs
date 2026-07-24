//! Error injection: sources that truncate mid-file and transports that
//! fail outright surface as the dedicated error variants.

use pdfboss_aio::{AsyncDocument, Backend, BoxFuture, Error, MemBackend};
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
