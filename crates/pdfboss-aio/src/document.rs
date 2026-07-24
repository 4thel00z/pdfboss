//! The async document model: opening fetches only the file tail, the xref
//! chain and the page-tree nodes; objects are fetched span-by-span through
//! growing windows and parsed by the sync core machinery. The whole file
//! is never read.

// This module's `pub(crate)` surface (Fetcher, find_tail, parse_version,
// header_span_in, ...) is exercised only by its own unit tests until Tasks
// 6-12 construct an `AsyncDocument` that wires them together.
#![allow(dead_code)] // TODO: remove once AsyncDocument (Task 6+) uses these

use std::sync::Arc;

use pdfboss_core::elements::Span;
use pdfboss_core::lexer::{Lexer, Token};

use crate::backend::Backend;
use crate::error::{Error, Result};

/// Initial tail window scanned for `startxref`, doubling per retry.
const TAIL_WINDOW: u64 = 4096;
/// Widest tail window tried before the chain is declared unusable.
const MAX_TAIL_WINDOW: u64 = 64 * 1024;

/// Bounded fetch helper: whole-range reads with truncation detection.
pub(crate) struct Fetcher {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) len: u64,
}

impl Fetcher {
    /// Reads exactly `[start, end)` (callers clamp to the file length). A
    /// read that stops short of `end` is reported as
    /// [`Error::TruncatedRead`] carrying the range being fetched.
    pub(crate) async fn read_range(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        let wanted = usize::try_from(end.saturating_sub(start)).map_err(|overflow| {
            Error::Core(pdfboss_core::Error::Other(format!(
                "range {start}..{end} does not fit this platform: {overflow}"
            )))
        })?;
        let mut buf = vec![0u8; wanted];
        let mut filled = 0;
        while filled < wanted {
            let got = self
                .backend
                .read_at(start + filled as u64, &mut buf[filled..])
                .await
                .map_err(Error::from)?;
            if got == 0 {
                return Err(Error::TruncatedRead {
                    offset: start,
                    wanted,
                    got: filled,
                });
            }
            filled += got;
        }
        Ok(buf)
    }

    /// Reads the window `[offset, offset + window)` clamped to the file
    /// end; an offset at or past the end yields an empty buffer.
    pub(crate) async fn window(&self, offset: u64, window: usize) -> Result<Vec<u8>> {
        let end = self.len.min(offset.saturating_add(window as u64));
        if offset >= end {
            return Ok(Vec::new());
        }
        self.read_range(offset, end).await
    }
}

/// Finds the first occurrence of `needle` in `haystack`.
pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Finds the last occurrence of `needle` in `haystack`.
pub(crate) fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// The file's final `startxref` announcement.
pub(crate) struct StartXrefRecord {
    /// The announced xref offset.
    pub(crate) offset: u64,
    /// Span of `startxref` through the offset integer.
    pub(crate) span: Span,
}

/// Locates the last `startxref` (and the last `%%EOF`) by scanning a
/// growing tail window: 4 KiB doubling to 64 KiB (ISO 32000 §7.5.5). No
/// whole-file recovery scan exists here — that would defeat the
/// never-read-the-whole-file guarantee — so an absent keyword is
/// `InvalidXref`.
pub(crate) async fn find_tail(fetcher: &Fetcher) -> Result<(StartXrefRecord, Option<Span>)> {
    let mut window = TAIL_WINDOW;
    loop {
        let start = fetcher.len.saturating_sub(window);
        let tail = fetcher.read_range(start, fetcher.len).await?;
        if let Some(rel) = rfind_bytes(&tail, b"startxref") {
            let mut lexer = Lexer::at(&tail, rel + b"startxref".len());
            if let Ok(Token::Int(value)) = lexer.next_token() {
                if value >= 0 && (value as u64) < fetcher.len {
                    let record = StartXrefRecord {
                        offset: value as u64,
                        span: Span {
                            start: start + rel as u64,
                            end: start + lexer.pos() as u64,
                        },
                    };
                    let eof = rfind_bytes(&tail, b"%%EOF").map(|pos| Span {
                        start: start + pos as u64,
                        end: start + pos as u64 + 5,
                    });
                    return Ok((record, eof));
                }
            }
        }
        if window >= fetcher.len || window >= MAX_TAIL_WINDOW {
            return Err(Error::Core(pdfboss_core::Error::InvalidXref));
        }
        window *= 2;
    }
}

/// Parses the `%PDF-x.y` header from the first bytes of the file,
/// scanning up to 1 KiB; absent or malformed headers default to 1.4,
/// mirroring the sync document model.
pub(crate) fn parse_version(head: &[u8]) -> (u8, u8) {
    try_parse_version(head).unwrap_or((1, 4))
}

fn try_parse_version(head: &[u8]) -> Option<(u8, u8)> {
    let window = &head[..head.len().min(1024)];
    let pos = find_bytes(window, b"%PDF-")?;
    let rest = &window[pos + 5..];
    let (major, used) = read_version_component(rest)?;
    if rest.get(used) != Some(&b'.') {
        return None;
    }
    let minor = read_version_component(&rest[used + 1..])?.0;
    Some((major, minor))
}

/// Reads a run of 1–3 ASCII digits as a `u8`, returning the value and the
/// number of bytes consumed.
fn read_version_component(bytes: &[u8]) -> Option<(u8, usize)> {
    let end = bytes
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(bytes.len());
    if end == 0 || end > 3 {
        return None;
    }
    let value = std::str::from_utf8(&bytes[..end]).ok()?.parse().ok()?;
    Some((value, end))
}

/// Span of the `%PDF-` header: match start through the run of version
/// characters (ASCII digits and dots) after it, scanning the first 1 KiB
/// (adopted rule 1, pinned by the core iterator). `None` when no header
/// exists — the Header element is simply omitted (lenient).
pub(crate) fn header_span_in(head: &[u8]) -> Option<Span> {
    let window = &head[..head.len().min(1024)];
    let pos = find_bytes(window, b"%PDF-")?;
    let version_end = window[pos + 5..]
        .iter()
        .position(|&b| !(b.is_ascii_digit() || b == b'.'))
        .map(|rel| pos + 5 + rel)
        .unwrap_or(window.len());
    Some(Span {
        start: pos as u64,
        end: version_end as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemBackend;
    use pdfboss_testkit::simple_doc;

    fn fetcher_for(data: Vec<u8>) -> Fetcher {
        let len = data.len() as u64;
        Fetcher {
            backend: std::sync::Arc::new(MemBackend::from(data)),
            len,
        }
    }

    /// Offset of the first occurrence of `needle` in `data`.
    fn pos_of(data: &[u8], needle: &[u8]) -> usize {
        data.windows(needle.len())
            .position(|w| w == needle)
            .expect("needle present")
    }

    #[tokio::test]
    async fn read_range_returns_exact_bytes_and_detects_truncation() {
        let fetcher = fetcher_for(b"0123456789".to_vec());
        assert_eq!(fetcher.read_range(2, 6).await.unwrap(), b"2345");
        assert_eq!(fetcher.window(8, 100).await.unwrap(), b"89");
        assert!(fetcher.window(10, 100).await.unwrap().is_empty());
        // A fetcher whose declared length exceeds the real data hits EOF
        // mid-range: TruncatedRead with the range it was fetching.
        let lying = Fetcher {
            backend: std::sync::Arc::new(MemBackend::from(b"0123456789".to_vec())),
            len: 20,
        };
        match lying.read_range(5, 15).await {
            Err(crate::Error::TruncatedRead {
                offset,
                wanted,
                got,
            }) => {
                assert_eq!(offset, 5);
                assert_eq!(wanted, 10);
                assert_eq!(got, 5);
            }
            other => panic!("expected TruncatedRead, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tail_scan_finds_startxref_and_eof() {
        let data = simple_doc("tail scan");
        let xref_pos = pos_of(&data, b"xref\n0 ") as u64;
        let startxref_pos = pos_of(&data, b"startxref") as u64;
        let eof_pos = pos_of(&data, b"%%EOF") as u64;
        let fetcher = fetcher_for(data);
        let (record, eof) = find_tail(&fetcher).await.unwrap();
        assert_eq!(record.offset, xref_pos);
        assert_eq!(record.span.start, startxref_pos);
        assert!(record.span.end > startxref_pos + b"startxref".len() as u64);
        assert_eq!(
            eof,
            Some(Span {
                start: eof_pos,
                end: eof_pos + 5
            })
        );
    }

    #[tokio::test]
    async fn tail_scan_grows_past_trailing_padding() {
        let mut data = simple_doc("padded");
        let xref_pos = pos_of(&data, b"xref\n0 ") as u64;
        data.extend_from_slice(&vec![b' '; 8192]);
        let fetcher = fetcher_for(data);
        let (record, eof) = find_tail(&fetcher).await.unwrap();
        assert_eq!(record.offset, xref_pos);
        assert!(eof.is_some());
    }

    #[tokio::test]
    async fn tail_scan_without_startxref_is_invalid_xref() {
        let fetcher = fetcher_for(b"not a pdf at all".to_vec());
        assert!(matches!(
            find_tail(&fetcher).await,
            Err(crate::Error::Core(pdfboss_core::Error::InvalidXref))
        ));
    }

    #[test]
    fn version_parse_matches_header_and_defaults() {
        assert_eq!(parse_version(b"%PDF-1.7\nrest"), (1, 7));
        assert_eq!(parse_version(b"junk\n%PDF-2.0\n"), (2, 0));
        assert_eq!(parse_version(b"%QQQ-1.7"), (1, 4));
        assert_eq!(parse_version(b""), (1, 4));
        assert_eq!(parse_version(b"%PDF-1."), (1, 4));
    }

    #[test]
    fn header_span_covers_the_version_run() {
        assert_eq!(
            header_span_in(b"%PDF-1.7\nrest"),
            Some(Span { start: 0, end: 8 })
        );
        assert_eq!(
            header_span_in(b"junk\n%PDF-2.0\n"),
            Some(Span { start: 5, end: 13 })
        );
        assert_eq!(header_span_in(b"%QQQ-1.7"), None);
        assert_eq!(header_span_in(b""), None);
    }
}
