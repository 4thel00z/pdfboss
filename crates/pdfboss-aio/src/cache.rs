//! Chunked LRU read cache over any backend: many small reads become few
//! chunk-sized fetches, and hot chunks stay resident up to a byte budget.
//! Default 64 KiB chunks, 32 MiB cap. Misses batch adaptively: a miss
//! landing near the previous one doubles the batch (up to 8 MiB), a far
//! one halves it, and each miss fetches its uncached neighborhood in one
//! inner read, so dense access over a high-latency backend collapses into
//! few large requests while scattered access stays at one chunk per miss.

use std::collections::HashMap;
use std::io;
use std::sync::Mutex;

use crate::backend::{Backend, BoxFuture};

/// Default chunk size: 64 KiB.
pub const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;
/// Default total cache capacity: 32 MiB.
pub const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;
/// Largest run of chunks one miss may fetch in a single inner read.
const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Chunked LRU read cache over any backend.
///
/// Reads are served chunk-by-chunk from an in-memory map; misses fetch a
/// batched run of chunks from the inner backend (see the module doc).
/// Concurrent misses on the same chunk may fetch it twice (both results
/// are identical; one wins the cache slot) — correctness is unaffected.
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
    /// Current batch: how many consecutive chunks the next miss fetches.
    batch_chunks: usize,
    /// Chunk index of the previous miss, the density reference point.
    previous_miss: Option<u64>,
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
                batch_chunks: 1,
                previous_miss: None,
            }),
            len: tokio::sync::OnceCell::new(),
        }
    }

    /// The chunk at `index`: from cache when resident (touching its LRU
    /// stamp), otherwise fetched from the inner backend as part of a
    /// batched run of chunks (all inserted, evicting least-recently-used
    /// chunks beyond the capacity).
    async fn chunk(&self, index: u64, file_len: u64) -> io::Result<Vec<u8>> {
        if let Some(hit) = self.lookup(index) {
            return Ok(hit);
        }
        let (first, run) = self.batch_run(index, file_len);
        let start = first * self.chunk_size as u64;
        let size = usize::try_from((file_len - start).min((run * self.chunk_size) as u64))
            .expect("batch size fits usize");
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
        let wanted = (index - first) as usize;
        let mut result = Vec::new();
        for (step, piece) in data.chunks(self.chunk_size).enumerate() {
            if step == wanted {
                result = piece.to_vec();
            }
            self.insert(first + step as u64, piece.to_vec());
        }
        if data.is_empty() {
            // The inner source produced nothing (shorter than declared):
            // record the empty chunk so the read loop sees a short read.
            self.insert(index, Vec::new());
        }
        Ok(result)
    }

    /// Decides the run of consecutive chunks fetched for the miss at
    /// `index`, as `(first_chunk, length)`. A miss within four times the
    /// current batch of the previous one is dense access and doubles the
    /// batch, up
    /// to [`MAX_BATCH_BYTES`] and a quarter of the cache budget (so one run
    /// can never evict what the reader is actively using); anything farther
    /// halves it, so an excursion does not throw away an established
    /// density estimate. The run is the uncached neighborhood of `index`:
    /// it grows downward first and then upward with the leftover budget,
    /// each direction stopping at a resident chunk or a file bound, so
    /// nothing is fetched twice and the run adapts by itself to walks that
    /// march backward (page objects laid out in descending file order are
    /// common), forward, or jitter around a moving locality.
    fn batch_run(&self, index: u64, file_len: u64) -> (u64, usize) {
        let max_chunks = (MAX_BATCH_BYTES.min(self.max_bytes / 4) / self.chunk_size).max(1);
        let mut state = self.state.lock().expect("cache mutex");
        let dense = state
            .previous_miss
            .is_some_and(|previous| index.abs_diff(previous) <= 4 * state.batch_chunks as u64);
        state.batch_chunks = if dense {
            (state.batch_chunks * 2).min(max_chunks)
        } else {
            (state.batch_chunks / 2).max(1)
        };
        state.previous_miss = Some(index);
        let chunks_total = file_len.div_ceil(self.chunk_size as u64);
        let mut budget = state.batch_chunks - 1;
        let mut first = index;
        while budget > 0 && first > 0 && !state.chunks.contains_key(&(first - 1)) {
            first -= 1;
            budget -= 1;
        }
        let mut last = index;
        while budget > 0 && last + 1 < chunks_total && !state.chunks.contains_key(&(last + 1)) {
            last += 1;
            budget -= 1;
        }
        (first, (last - first + 1) as usize)
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

    /// Inner-fetch tallies: how many read_at calls, and how many bytes they
    /// actually returned.
    #[derive(Default)]
    struct Counts {
        calls: AtomicUsize,
        bytes: AtomicUsize,
    }

    /// Delegates to a MemBackend while tallying inner fetches.
    struct CountingBackend {
        inner: MemBackend,
        counts: Arc<Counts>,
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
            self.counts.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let count = self.inner.read_at(offset, buf).await?;
                self.counts.bytes.fetch_add(count, Ordering::SeqCst);
                Ok(count)
            })
        }
    }

    fn counting(data: Vec<u8>) -> (CountingBackend, Arc<Counts>) {
        let counts = Arc::new(Counts::default());
        (
            CountingBackend {
                inner: MemBackend::from(data),
                counts: Arc::clone(&counts),
            },
            counts,
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
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reads_spanning_chunks_and_eof_are_stitched() {
        let (inner, fetches) = counting((0u8..=255).collect());
        let cached = CachedBackend::with_capacity(inner, 64, 1024);
        let mut buf = [0u8; 100];
        // Bytes 30..=129 span chunks 0 (0..63), 1 (64..127) and 2 (128..191).
        // The spanning read is dense by construction: the second miss
        // batches chunks 1 and 2 into one inner fetch.
        assert_eq!(cached.read_at(30, &mut buf).await.unwrap(), 100);
        assert_eq!(buf[0], 30);
        assert_eq!(buf[99], 129);
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 2);
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
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 3);
        cached.read_at(0, &mut buf).await.unwrap(); // still cached
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 3);
        cached.read_at(64, &mut buf).await.unwrap(); // refetched
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 4);
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
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 2);
        cached.read_at(1000, &mut buf).await.unwrap();
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dense_scan_escalates_batches_and_collapses_fetches() {
        // 64 chunks of 64 bytes, read end to end in chunk-sized steps.
        let data: Vec<u8> = (0..4096u32).map(|v| v as u8).collect();
        let (inner, fetches) = counting(data.clone());
        let cached = CachedBackend::with_capacity(inner, 64, 1 << 20);
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        for step in 0..64 {
            let count = cached.read_at(step * 64, &mut buf).await.unwrap();
            got.extend_from_slice(&buf[..count]);
        }
        assert_eq!(got, data, "batched fetches must not corrupt the bytes");
        // Miss runs double while the scan stays dense: 1, 1, 2, 4, 8, 16,
        // 32, then the tail. 64 chunks arrive in 7 fetches instead of 64.
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 7);
    }

    #[tokio::test]
    async fn backward_scan_escalates_batches_toward_lower_offsets() {
        // The same 64 chunks read back to front, the layout the Tafsir
        // trace showed live: runs must extend backward, ending at the
        // missed chunk, or every escalated fetch covers the wrong side.
        let data: Vec<u8> = (0..4096u32).map(|v| v as u8).collect();
        let (inner, fetches) = counting(data.clone());
        let cached = CachedBackend::with_capacity(inner, 64, 1 << 20);
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        for step in (0..64u64).rev() {
            let count = cached.read_at(step * 64, &mut buf).await.unwrap();
            got.splice(0..0, buf[..count].iter().copied());
        }
        assert_eq!(got, data, "backward batched fetches must not corrupt");
        // Mirror of the forward ladder: 1, 1, 2, 4, 8, 16, 32, tail.
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 7);
        assert_eq!(fetches.bytes.load(Ordering::SeqCst), 4096, "each byte once");
    }

    #[tokio::test]
    async fn far_jumps_reset_the_batch_to_one_chunk() {
        let (inner, fetches) = counting(vec![9u8; 8192]);
        let cached = CachedBackend::with_capacity(inner, 64, 1 << 20);
        let mut buf = [0u8; 16];
        // Three scattered reads, each far outside twice the current batch:
        // every one is a single-chunk fetch, no overshoot.
        cached.read_at(0, &mut buf).await.unwrap();
        cached.read_at(4096, &mut buf).await.unwrap();
        cached.read_at(1024, &mut buf).await.unwrap();
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 3);
        // Re-reads of those offsets stay hits: nothing beyond one chunk
        // per miss was fetched, so the neighbors were never pulled in.
        cached.read_at(64, &mut buf).await.unwrap();
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn batch_runs_trim_at_cached_chunks_and_eof() {
        let data: Vec<u8> = (0..4096u32).map(|v| v as u8).collect();
        let (inner, fetches) = counting(data);
        let cached = CachedBackend::with_capacity(inner, 64, 1 << 20);
        let mut buf = [0u8; 16];
        cached.read_at(5 * 64, &mut buf).await.unwrap(); // miss 5, run [5]
        cached.read_at(2 * 64, &mut buf).await.unwrap(); // dense: run [1,2]
                                                         // Dense miss at 4 with batch 4: the neighborhood is walled in by
                                                         // the resident chunks 2 and 5, so one fetch covers exactly [3, 4]
                                                         // and the next read is a hit.
        cached.read_at(4 * 64, &mut buf).await.unwrap();
        cached.read_at(3 * 64, &mut buf).await.unwrap();
        assert_eq!(fetches.calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            fetches.bytes.load(Ordering::SeqCst),
            5 * 64,
            "chunks 5, 1-2 and 3-4 fetched exactly once, nothing beyond"
        );
        // A run near the end of the file trims at EOF and stays correct.
        let mut tail = [0u8; 64];
        assert_eq!(cached.read_at(4032, &mut tail).await.unwrap(), 64);
        assert_eq!(tail[63], 4095u32 as u8);
    }

    #[test]
    fn duplicate_insert_does_not_double_count() {
        // Create a cache to test byte accounting
        let (inner, _) = counting(vec![0u8; 200]);
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
