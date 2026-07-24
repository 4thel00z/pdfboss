//! Chunked LRU read cache over any backend: many small reads become few
//! chunk-sized fetches, and hot chunks stay resident up to a byte budget.
//! Default 64 KiB chunks, 32 MiB cap.

use std::collections::HashMap;
use std::io;
use std::sync::Mutex;

use crate::backend::{Backend, BoxFuture};

/// Default chunk size: 64 KiB.
pub const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;
/// Default total cache capacity: 32 MiB.
pub const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;

/// Chunked LRU read cache over any backend.
///
/// Reads are served chunk-by-chunk from an in-memory map; misses fetch the
/// whole containing chunk from the inner backend. Concurrent misses on the
/// same chunk may fetch it twice (both results are identical; one wins the
/// cache slot) — correctness is unaffected.
pub struct CachedBackend<B: Backend> {
    inner: B,
    chunk_size: usize,
    max_bytes: usize,
    state: Mutex<CacheState>,
    len: tokio::sync::OnceCell<u64>,
}

struct CacheState {
    chunks: HashMap<u64, CacheEntry>,
    bytes: usize,
    clock: u64,
}

struct CacheEntry {
    data: Vec<u8>,
    stamp: u64,
}

impl<B: Backend> CachedBackend<B> {
    /// Wraps `inner` with the default 64 KiB chunks and 32 MiB capacity.
    pub fn new(inner: B) -> Self {
        Self::with_capacity(inner, DEFAULT_CHUNK_SIZE, DEFAULT_MAX_BYTES)
    }

    /// Wraps `inner` with an explicit chunk size and total byte capacity.
    ///
    /// # Panics
    /// Panics if `chunk_size` is zero.
    pub fn with_capacity(inner: B, chunk_size: usize, max_bytes: usize) -> Self {
        assert!(chunk_size > 0, "chunk_size must be nonzero");
        CachedBackend {
            inner,
            chunk_size,
            max_bytes,
            state: Mutex::new(CacheState {
                chunks: HashMap::new(),
                bytes: 0,
                clock: 0,
            }),
            len: tokio::sync::OnceCell::new(),
        }
    }

    /// The chunk at `index`: from cache when resident (touching its LRU
    /// stamp), otherwise fetched whole from the inner backend and inserted,
    /// evicting least-recently-used chunks beyond the capacity.
    async fn chunk(&self, index: u64, file_len: u64) -> io::Result<Vec<u8>> {
        if let Some(hit) = self.lookup(index) {
            return Ok(hit);
        }
        let start = index * self.chunk_size as u64;
        let size = usize::try_from((file_len - start).min(self.chunk_size as u64))
            .expect("chunk size fits usize");
        let mut data = vec![0u8; size];
        let mut filled = 0;
        while filled < size {
            let count = self
                .inner
                .read_at(start + filled as u64, &mut data[filled..])
                .await?;
            if count == 0 {
                break;
            }
            filled += count;
        }
        data.truncate(filled);
        self.insert(index, data.clone());
        Ok(data)
    }

    /// Cache lookup, refreshing the entry's recency stamp on a hit.
    fn lookup(&self, index: u64) -> Option<Vec<u8>> {
        let mut state = self.state.lock().expect("cache mutex");
        state.clock += 1;
        let stamp = state.clock;
        let entry = state.chunks.get_mut(&index)?;
        entry.stamp = stamp;
        Some(entry.data.clone())
    }

    /// Inserts a chunk, evicting the least-recently-used entries until the
    /// total stays within `max_bytes`.
    fn insert(&self, index: u64, data: Vec<u8>) {
        let mut state = self.state.lock().expect("cache mutex");
        state.clock += 1;
        let stamp = state.clock;
        state.bytes += data.len();
        let old_entry = state.chunks.insert(index, CacheEntry { data, stamp });
        if let Some(old) = old_entry {
            state.bytes -= old.data.len();
        }
        while state.bytes > self.max_bytes && state.chunks.len() > 1 {
            let coldest = state
                .chunks
                .iter()
                .filter(|(candidate, _)| **candidate != index)
                .min_by_key(|(_, entry)| entry.stamp)
                .map(|(candidate, _)| *candidate);
            match coldest {
                Some(victim) => {
                    if let Some(gone) = state.chunks.remove(&victim) {
                        state.bytes -= gone.data.len();
                    }
                }
                None => break,
            }
        }
    }
}

impl<B: Backend> Backend for CachedBackend<B> {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>> {
        Box::pin(async move { self.len.get_or_try_init(|| self.inner.len()).await.copied() })
    }

    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8]) -> BoxFuture<'a, io::Result<usize>> {
        Box::pin(async move {
            let file_len = self.len().await?;
            if offset >= file_len || buf.is_empty() {
                return Ok(0);
            }
            let available = usize::try_from(file_len - offset).unwrap_or(usize::MAX);
            let want = buf.len().min(available);
            let mut done = 0;
            while done < want {
                let pos = offset + done as u64;
                let index = pos / self.chunk_size as u64;
                let within = (pos % self.chunk_size as u64) as usize;
                let chunk = self.chunk(index, file_len).await?;
                if within >= chunk.len() {
                    break; // inner source shorter than its declared length
                }
                let count = (want - done).min(chunk.len() - within);
                buf[done..done + count].copy_from_slice(&chunk[within..within + count]);
                done += count;
            }
            Ok(done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemBackend;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Delegates to a MemBackend while counting inner read_at calls.
    struct CountingBackend {
        inner: MemBackend,
        fetches: Arc<AtomicUsize>,
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
            self.fetches.fetch_add(1, Ordering::SeqCst);
            self.inner.read_at(offset, buf)
        }
    }

    fn counting(data: Vec<u8>) -> (CountingBackend, Arc<AtomicUsize>) {
        let fetches = Arc::new(AtomicUsize::new(0));
        (
            CountingBackend {
                inner: MemBackend::from(data),
                fetches: Arc::clone(&fetches),
            },
            fetches,
        )
    }

    #[tokio::test]
    async fn repeated_reads_fetch_each_chunk_once() {
        let (inner, fetches) = counting((0u8..=255).collect());
        let cached = CachedBackend::with_capacity(inner, 64, 1024);
        let mut buf = [0u8; 8];
        assert_eq!(cached.read_at(10, &mut buf).await.unwrap(), 8);
        assert_eq!(&buf, &[10, 11, 12, 13, 14, 15, 16, 17]);
        assert_eq!(cached.read_at(20, &mut buf).await.unwrap(), 8);
        assert_eq!(&buf, &[20, 21, 22, 23, 24, 25, 26, 27]);
        // Both reads live in chunk 0: exactly one inner fetch.
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reads_spanning_chunks_and_eof_are_stitched() {
        let (inner, fetches) = counting((0u8..=255).collect());
        let cached = CachedBackend::with_capacity(inner, 64, 1024);
        let mut buf = [0u8; 100];
        // Bytes 30..=129 span chunks 0 (0..63), 1 (64..127) and 2 (128..191).
        assert_eq!(cached.read_at(30, &mut buf).await.unwrap(), 100);
        assert_eq!(buf[0], 30);
        assert_eq!(buf[99], 129);
        assert_eq!(fetches.load(Ordering::SeqCst), 3);
        // Short read at EOF (len 256).
        assert_eq!(cached.read_at(250, &mut buf).await.unwrap(), 6);
        assert_eq!(&buf[..6], &[250, 251, 252, 253, 254, 255]);
        assert_eq!(cached.read_at(256, &mut buf).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn lru_evicts_the_coldest_chunk() {
        let (inner, fetches) = counting((0u8..=255).collect());
        // Capacity for exactly two 64-byte chunks.
        let cached = CachedBackend::with_capacity(inner, 64, 128);
        let mut buf = [0u8; 4];
        cached.read_at(0, &mut buf).await.unwrap(); // chunk 0
        cached.read_at(64, &mut buf).await.unwrap(); // chunk 1
        cached.read_at(0, &mut buf).await.unwrap(); // touch chunk 0
        cached.read_at(128, &mut buf).await.unwrap(); // chunk 2 evicts chunk 1
        assert_eq!(fetches.load(Ordering::SeqCst), 3);
        cached.read_at(0, &mut buf).await.unwrap(); // still cached
        assert_eq!(fetches.load(Ordering::SeqCst), 3);
        cached.read_at(64, &mut buf).await.unwrap(); // refetched
        assert_eq!(fetches.load(Ordering::SeqCst), 4);
        assert_eq!(&buf, &[64, 65, 66, 67]);
    }

    #[tokio::test]
    async fn default_capacity_uses_64_kib_chunks() {
        let (inner, fetches) = counting(vec![7u8; 200_000]);
        let cached = CachedBackend::new(inner);
        let mut buf = [0u8; 16];
        cached.read_at(0, &mut buf).await.unwrap();
        cached.read_at(70_000, &mut buf).await.unwrap();
        // 0 lives in chunk 0, 70_000 (past the 65_536-byte boundary) in
        // chunk 1 of the 64 KiB grid: two inner fetches, and a re-read of
        // either offset adds none.
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        cached.read_at(1000, &mut buf).await.unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn duplicate_insert_does_not_double_count() {
        // Create a cache to test byte accounting
        let (inner, _fetches) = counting(vec![0u8; 200]);
        let cached = CachedBackend::with_capacity(inner, 64, 256);

        // Insert the same chunk twice directly
        cached.insert(0, vec![0u8; 64]);
        cached.insert(0, vec![0u8; 64]);

        // Check that bytes is still 64, not 128
        let state = cached.state.lock().unwrap();
        assert_eq!(
            state.bytes, 64,
            "Duplicate insert should not double-count bytes"
        );
        assert_eq!(state.chunks.len(), 1, "Should have exactly one chunk");
    }
}
