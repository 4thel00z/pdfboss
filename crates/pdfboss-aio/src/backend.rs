//! Random-access byte sources: in-memory bytes, positioned file reads on a
//! blocking thread, and (behind the `http` feature) remote HTTP range
//! requests with a one-time full-download fallback for range-less servers.
//! The trait is object-safe — futures are boxed — so documents can hold
//! `Arc<dyn Backend>`.

use std::io;
use std::path::Path;
use std::sync::Arc;

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

/// One bounded read from an in-memory byte source: offsets at or past the
/// end read zero bytes, everything else fills as much of `buf` as the data
/// allows.
fn read_from_bytes(data: &[u8], offset: u64, buf: &mut [u8]) -> usize {
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(data.len());
    let count = buf.len().min(data.len() - start);
    buf[..count].copy_from_slice(&data[start..start + count]);
    count
}

impl Backend for MemBackend {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        let total = self.0.len() as u64;
        Box::pin(async move { Ok(total) })
    }

    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8]) -> BoxFuture<'a, io::Result<usize>> {
        Box::pin(async move { Ok(read_from_bytes(&self.0, offset, buf)) })
    }
}

/// A byte source backed by a file. Reads run as positioned reads on a
/// blocking thread pool so the async runtime is never stalled by disk I/O;
/// the length is captured once at open (the file is treated as immutable
/// while the backend lives).
pub struct FileBackend {
    file: Arc<std::fs::File>,
    len: u64,
}

impl FileBackend {
    /// Opens `path` and records its current length.
    pub async fn open(path: impl AsRef<Path>) -> io::Result<FileBackend> {
        let path = path.as_ref().to_owned();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(path)?;
            let len = file.metadata()?.len();
            Ok(FileBackend {
                file: Arc::new(file),
                len,
            })
        })
        .await
        .map_err(io::Error::other)?
    }
}

/// One positioned read at `offset` (no shared cursor).
fn positioned_read(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        file.seek_read(buf, offset)
    }
}

/// Loops positioned reads over short counts so callers only ever see a
/// short total at end of file.
fn read_at_fully(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let count = positioned_read(file, offset + filled as u64, &mut buf[filled..])?;
        if count == 0 {
            break;
        }
        filled += count;
    }
    Ok(filled)
}

impl Backend for FileBackend {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        let total = self.len;
        Box::pin(async move { Ok(total) })
    }

    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8]) -> BoxFuture<'a, io::Result<usize>> {
        let file = Arc::clone(&self.file);
        let wanted = buf.len();
        Box::pin(async move {
            let chunk = tokio::task::spawn_blocking(move || {
                let mut scratch = vec![0u8; wanted];
                let count = read_at_fully(&file, offset, &mut scratch)?;
                scratch.truncate(count);
                Ok::<Vec<u8>, io::Error>(scratch)
            })
            .await
            .map_err(io::Error::other)??;
            buf[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
        })
    }
}

/// A byte source over HTTP: length via `HEAD`/`Content-Length`, reads via
/// `Range: bytes=` requests. A server that ignores Range (answers 200 with
/// the full body instead of 206, like `python3 -m http.server`) triggers a
/// one-time fallback: that very response body is the whole resource, so it
/// is collected into memory (capped at the declared length) and every read
/// is served from it. Range-less servers cost one full download held
/// resident instead of failing.
#[cfg(feature = "http")]
pub struct HttpBackend {
    client: reqwest::Client,
    url: reqwest::Url,
    len: u64,
    full: std::sync::OnceLock<Bytes>,
    progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
}

#[cfg(feature = "http")]
impl HttpBackend {
    /// Issues a `HEAD` request to learn the resource length.
    pub async fn new(url: impl reqwest::IntoUrl) -> crate::Result<HttpBackend> {
        let url = url.into_url().map_err(|err| crate::Error::Http {
            status: None,
            msg: err.to_string(),
        })?;
        let client = reqwest::Client::new();
        let response = client
            .head(url.clone())
            .send()
            .await
            .map_err(|err| crate::Error::Http {
                status: err.status().map(|status| status.as_u16()),
                msg: err.to_string(),
            })?;
        if !response.status().is_success() {
            return Err(crate::Error::Http {
                status: Some(response.status().as_u16()),
                msg: format!("HEAD {url} failed"),
            });
        }
        let len = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| crate::Error::Http {
                status: Some(response.status().as_u16()),
                msg: format!("HEAD {url}: missing or malformed Content-Length"),
            })?;
        Ok(HttpBackend {
            client,
            url,
            len,
            full: std::sync::OnceLock::new(),
            progress: None,
        })
    }

    /// Registers a fallback-download observer: when a range-ignoring server
    /// forces the one-time full download, `progress(collected, declared)` is
    /// called once before the first byte (`collected == 0`) and after every
    /// received chunk, with `declared` the HEAD-declared total. Ranged reads
    /// against a range-honoring server never call it. The callback runs on
    /// the async runtime, so it must not block.
    pub fn on_fallback_progress(
        mut self,
        progress: impl Fn(u64, u64) + Send + Sync + 'static,
    ) -> HttpBackend {
        self.progress = Some(Arc::new(progress));
        self
    }

    /// Collects a 200 response body (the whole resource) capped at the
    /// declared length, so a hostile (or merely buggy) server whose body
    /// is larger, or never finishes, cannot grow memory past `len` or
    /// stall the read; a body that ends short of `len` is kept as-is and
    /// later reads past it come back short.
    async fn collect_full_body(&self, response: reqwest::Response) -> io::Result<Bytes> {
        use futures_util::StreamExt;
        let cap = usize::try_from(self.len).unwrap_or(usize::MAX);
        let mut collected = Vec::new();
        let mut chunks = response.bytes_stream();
        if let Some(progress) = &self.progress {
            progress(0, self.len);
        }
        while collected.len() < cap {
            let chunk = match chunks.next().await {
                Some(Ok(chunk)) => chunk,
                Some(Err(err)) => {
                    return Err(http_io_error(crate::error::TransportMarker {
                        status: None,
                        msg: format!("GET {}: {err}", self.url),
                    }))
                }
                None => break, // body ended short of the declared length
            };
            let take = (cap - collected.len()).min(chunk.len());
            collected.extend_from_slice(&chunk[..take]);
            if let Some(progress) = &self.progress {
                progress(collected.len() as u64, self.len);
            }
        }
        Ok(Bytes::from(collected))
    }
}

/// Wraps a transport marker into `io::Error` so it can cross the
/// `io::Result` boundary of the [`Backend`] trait; recovered by
/// `From<std::io::Error> for crate::Error`.
#[cfg(feature = "http")]
fn http_io_error(marker: crate::error::TransportMarker) -> io::Error {
    io::Error::other(marker)
}

#[cfg(feature = "http")]
impl Backend for HttpBackend {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        let total = self.len;
        Box::pin(async move { Ok(total) })
    }

    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8]) -> BoxFuture<'a, io::Result<usize>> {
        Box::pin(async move {
            if offset >= self.len || buf.is_empty() {
                return Ok(0);
            }
            if let Some(body) = self.full.get() {
                return Ok(read_from_bytes(body, offset, buf));
            }
            let last = (offset + buf.len() as u64 - 1).min(self.len - 1);
            let response = self
                .client
                .get(self.url.clone())
                .header(reqwest::header::RANGE, format!("bytes={offset}-{last}"))
                .send()
                .await
                .map_err(|err| {
                    http_io_error(crate::error::TransportMarker {
                        status: err.status().map(|status| status.as_u16()),
                        msg: format!("GET {} range {offset}-{last}: {err}", self.url),
                    })
                })?;
            match response.status().as_u16() {
                206 => {}
                200 => {
                    // The server ignored the Range header and answered with
                    // the whole resource: keep it and serve every read,
                    // this one included, from memory. Losing the OnceLock
                    // race to a concurrent read just drops a redundant
                    // body, the same stance CachedBackend documents for
                    // concurrent chunk misses.
                    let collected = self.collect_full_body(response).await?;
                    let body = self.full.get_or_init(|| collected);
                    return Ok(read_from_bytes(body, offset, buf));
                }
                status => {
                    return Err(http_io_error(crate::error::TransportMarker {
                        status: Some(status),
                        msg: format!("GET {} range {offset}-{last} failed", self.url),
                    }));
                }
            }
            // Collect at most `buf.len()` bytes of the body: a hostile (or
            // merely buggy) server may answer a small Range with an
            // arbitrarily large — or never-finishing — body, so the
            // response is read chunk by chunk and collection stops the
            // moment `buf` is full. The remaining body (and its
            // connection) is simply dropped, never buffered; a body
            // shorter than `buf` yields a short read (handled by the
            // caller, `Fetcher::read_range`, exactly like any other
            // short read).
            use futures_util::StreamExt;
            let mut chunks = response.bytes_stream();
            let mut filled = 0usize;
            while filled < buf.len() {
                let chunk = match chunks.next().await {
                    Some(Ok(chunk)) => chunk,
                    Some(Err(err)) => {
                        return Err(http_io_error(crate::error::TransportMarker {
                            status: None,
                            msg: format!("GET {} range {offset}-{last}: {err}", self.url),
                        }))
                    }
                    None => break, // body ended short of the requested range
                };
                let take = (buf.len() - filled).min(chunk.len());
                buf[filled..filled + take].copy_from_slice(&chunk[..take]);
                filled += take;
            }
            Ok(filled)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn file_backend_positioned_reads() {
        let path = std::env::temp_dir().join(format!(
            "pdfboss-aio-backend-test-{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, b"0123456789abcdef").unwrap();
        let backend = FileBackend::open(&path).await.unwrap();
        assert_eq!(backend.len().await.unwrap(), 16);
        let mut buf = [0u8; 4];
        assert_eq!(backend.read_at(10, &mut buf).await.unwrap(), 4);
        assert_eq!(&buf, b"abcd");
        // Reads are positioned, not cursor-based: an earlier offset after a
        // later one must still return the right bytes.
        assert_eq!(backend.read_at(0, &mut buf).await.unwrap(), 4);
        assert_eq!(&buf, b"0123");
        // Short read only at end of file.
        let mut long = [0u8; 32];
        assert_eq!(backend.read_at(12, &mut long).await.unwrap(), 4);
        assert_eq!(&long[..4], b"cdef");
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn file_backend_open_missing_file_errors() {
        let missing = std::env::temp_dir().join("pdfboss-aio-backend-test-missing.bin");
        assert!(FileBackend::open(&missing).await.is_err());
    }

    #[cfg(feature = "http")]
    #[test]
    fn transport_marker_round_trips_through_io_error() {
        let failed = http_io_error(crate::error::TransportMarker {
            status: Some(503),
            msg: "unavailable".to_string(),
        });
        assert!(matches!(
            crate::Error::from(failed),
            crate::Error::Http {
                status: Some(503),
                ..
            }
        ));
    }
}
