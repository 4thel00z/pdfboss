//! The async document model: opening fetches only the file tail, the xref
//! chain and the page-tree nodes; objects are fetched span-by-span through
//! growing windows and parsed by the sync core machinery. The whole file
//! is never read.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use pdfboss_core::elements::{Span, XrefKind};
use pdfboss_core::lexer::{Lexer, Token};
use pdfboss_core::parser::{NoResolve, Parser};
use pdfboss_core::xref::XrefEntry;
use pdfboss_core::{Dict, Object};

use crate::backend::{Backend, FileBackend, MemBackend};
use crate::cache::CachedBackend;
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
    /// Span of `startxref` through the offset integer. Exposed by a
    /// `startxref_record()` accessor consumed by the element stream's
    /// physical layer (Plan 02 task 11).
    #[allow(dead_code)]
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

/// Bytes of slack demanded beyond a window parse end so trailing-keyword
/// lookahead (`endobj`, `endstream`, `trailer` dict close) can never be cut
/// mid-token by the window edge.
pub(crate) const PARSE_SLACK: usize = 16;

/// One cross-reference section as found while walking the chain.
#[derive(Clone)]
pub(crate) struct SectionRecord {
    /// Consumed, with `span` and `entries` below, by the element stream's
    /// physical layer building `Element::XrefSection` (Plan 02 task 11).
    #[allow(dead_code)]
    pub(crate) kind: XrefKind,
    /// Classic: `xref` keyword to the `trailer` keyword. Stream: the whole
    /// xref-stream object.
    #[allow(dead_code)] // see `kind` above (Plan 02 task 11)
    pub(crate) span: Span,
    /// Number of entries the section declares (subsection sums).
    #[allow(dead_code)] // see `kind` above (Plan 02 task 11)
    pub(crate) entries: usize,
    /// The section's trailer dictionary (classic trailer, or the stream's
    /// own dictionary).
    pub(crate) trailer_dict: Dict,
    /// Classic: `trailer` keyword through the dictionary. Stream: same as
    /// `span`.
    pub(crate) trailer_span: Span,
}

/// A parsed section plus everything the chain walk needs from it.
pub(crate) struct ParsedSection {
    pub(crate) record: SectionRecord,
    pub(crate) entries: Vec<(u32, XrefEntry)>,
    pub(crate) prev: Option<u64>,
    pub(crate) xrefstm: Option<u64>,
}

/// Parses the cross-reference section at absolute offset `base` from a
/// fetched window (ISO 32000 §7.5.4 classic tables, §7.5.8 xref streams).
/// `Ok(None)` means the window ended inside the section and the caller
/// must fetch a wider one; hard errors are reserved for input that no
/// wider window could fix.
pub(crate) fn parse_section_window(
    buf: &[u8],
    base: u64,
    file_len: u64,
    at_eof: bool,
) -> Result<Option<ParsedSection>> {
    let mut probe = Lexer::new(buf);
    let classic = matches!(probe.peek_token(),
                           Ok(Token::Keyword(ref k)) if k.as_slice() == b"xref");
    if classic {
        parse_classic_window(buf, base, file_len, at_eof)
    } else {
        parse_stream_window(buf, base, at_eof)
    }
}

/// Classic table: `xref`, subsections of `start count` then `count` entry
/// lines of `offset gen n|f` (read token-wise, so 19/20/21-byte entry
/// lines all load), then `trailer` and its dictionary.
fn parse_classic_window(
    buf: &[u8],
    base: u64,
    file_len: u64,
    at_eof: bool,
) -> Result<Option<ParsedSection>> {
    /// Maps a mid-window lex/parse failure to "need more bytes" unless the
    /// window already reaches the end of the file.
    fn incomplete<T>(at_eof: bool) -> Result<Option<T>> {
        if at_eof {
            Err(Error::Core(pdfboss_core::Error::InvalidXref))
        } else {
            Ok(None)
        }
    }

    let mut lexer = Lexer::new(buf);
    match lexer.next_token() {
        Ok(Token::Keyword(ref k)) if k.as_slice() == b"xref" => {}
        _ => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
    }
    let mut entries: Vec<(u32, XrefEntry)> = Vec::new();
    loop {
        lexer.skip_whitespace_and_comments();
        let keyword_start = lexer.pos();
        let token = match lexer.next_token() {
            Ok(t) => t,
            Err(_) => return incomplete(at_eof),
        };
        match token {
            Token::Int(start) if start >= 0 => {
                let count = match lexer.next_token() {
                    Ok(Token::Int(c)) if c >= 0 => c as u64,
                    Ok(Token::Eof) => return incomplete(at_eof),
                    Ok(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
                    Err(_) => return incomplete(at_eof),
                };
                // Even a degenerate entry line needs at least 11 bytes, so
                // a count beyond this bound cannot be real regardless of
                // how wide the window grows: hard error.
                if count > file_len / 11 + 1 {
                    return Err(Error::Core(pdfboss_core::Error::InvalidXref));
                }
                for i in 0..count {
                    let field1 = match lexer.next_token() {
                        Ok(Token::Int(v)) if v >= 0 => v as u64,
                        Ok(Token::Eof) => return incomplete(at_eof),
                        Ok(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
                        Err(_) => return incomplete(at_eof),
                    };
                    let field2 = match lexer.next_token() {
                        Ok(Token::Int(v)) if v >= 0 => v,
                        Ok(Token::Eof) => return incomplete(at_eof),
                        Ok(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
                        Err(_) => return incomplete(at_eof),
                    };
                    let entry = match lexer.next_token() {
                        Ok(Token::Keyword(ref k)) if k.as_slice() == b"n" => XrefEntry::InFile {
                            offset: field1,
                            gen: field2.min(65535) as u16,
                        },
                        Ok(Token::Keyword(ref k)) if k.as_slice() == b"f" => XrefEntry::Free,
                        Ok(Token::Eof) => return incomplete(at_eof),
                        Ok(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
                        Err(_) => return incomplete(at_eof),
                    };
                    if let Ok(num) = u32::try_from(start as u64 + i) {
                        entries.push((num, entry));
                    }
                }
                // An entry group flush with the window edge may itself be
                // truncated mid-number: demand slack before trusting it.
                if lexer.pos() + PARSE_SLACK > buf.len() && !at_eof {
                    return Ok(None);
                }
            }
            Token::Keyword(ref k) if k.as_slice() == b"trailer" => {
                let mut parser = Parser::at(buf, lexer.pos());
                let trailer_dict = match parser.parse_object(&NoResolve) {
                    Ok(Object::Dict(d)) => d,
                    Ok(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
                    Err(_) => return incomplete(at_eof),
                };
                if parser.pos() + PARSE_SLACK > buf.len() && !at_eof {
                    return Ok(None); // the dict may have been cut leniently
                }
                let prev = trailer_dict.get_int("Prev").and_then(non_negative);
                let xrefstm = trailer_dict.get_int("XRefStm").and_then(non_negative);
                let entry_count = entries.len();
                return Ok(Some(ParsedSection {
                    record: SectionRecord {
                        kind: XrefKind::Table,
                        span: Span {
                            start: base,
                            end: base + keyword_start as u64,
                        },
                        entries: entry_count,
                        trailer_dict,
                        trailer_span: Span {
                            start: base + keyword_start as u64,
                            end: base + parser.pos() as u64,
                        },
                    },
                    entries,
                    prev,
                    xrefstm,
                }));
            }
            Token::Eof => return incomplete(at_eof),
            _ => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
        }
    }
}

/// Cross-reference stream: an indirect stream object whose decoded data
/// holds fixed-width big-endian fields laid out per `/W`; a zero-width
/// type field defaults to type 1, `/Index` defaults to `[0 Size]`, and the
/// stream's own dictionary is the section trailer.
fn parse_stream_window(buf: &[u8], base: u64, at_eof: bool) -> Result<Option<ParsedSection>> {
    let mut parser = Parser::at(buf, 0);
    let stream = match parser.parse_indirect(&NoResolve) {
        Ok((_, Object::Stream(s))) => s,
        // Core's dict parser leniently breaks on `Eof` instead of erroring,
        // so a window cut inside the dictionary (before the `stream`
        // keyword is even reached) parses as a plain `Object::Dict` sitting
        // flush with the window edge: ask for more bytes rather than
        // hard-erroring, unless the window already reaches file end.
        Ok(_) => {
            if !at_eof && parser.pos() + PARSE_SLACK > buf.len() {
                return Ok(None);
            }
            return Err(Error::Core(pdfboss_core::Error::InvalidXref));
        }
        Err(_) if !at_eof => return Ok(None),
        Err(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
    };
    if parser.pos() + PARSE_SLACK > buf.len() && !at_eof {
        return Ok(None); // stream data may have been cut leniently
    }
    // Trust a declared /Length only when the parsed data honors it — a
    // window cut inside the stream falls into the lenient recovery path
    // and must grow instead. A missing or unresolvable (indirect) /Length
    // carries no verifiable bound at all, so it can only be accepted once
    // the window reaches file end.
    match stream.dict.get_int("Length") {
        Some(declared) if declared >= 0 => {
            if stream.data.len() as u64 != declared as u64 && !at_eof {
                return Ok(None);
            }
        }
        _ => {
            if !at_eof {
                return Ok(None);
            }
        }
    }
    let decoded = pdfboss_core::filters::decode_stream(&stream, &NoResolve)
        .map_err(|_| Error::Core(pdfboss_core::Error::InvalidXref))?;
    let dict = stream.dict;
    let widths: Vec<usize> = dict
        .get_array("W")
        .ok_or(Error::Core(pdfboss_core::Error::InvalidXref))?
        .iter()
        .map(|v| {
            v.as_int()
                .filter(|&n| (0..=8).contains(&n))
                .map(|n| n as usize)
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(Error::Core(pdfboss_core::Error::InvalidXref))?;
    let w1 = widths.first().copied().unwrap_or(0);
    let w2 = widths.get(1).copied().unwrap_or(0);
    let w3 = widths.get(2).copied().unwrap_or(0);
    let entry_len = w1 + w2 + w3;
    if entry_len == 0 {
        return Err(Error::Core(pdfboss_core::Error::InvalidXref));
    }
    let size = dict.get_int("Size").unwrap_or(0).max(0) as u64;
    let subsections: Vec<(u64, u64)> = match dict.get_array("Index") {
        Some(index) => index
            .chunks(2)
            .filter_map(|pair| {
                let start = pair.first()?.as_int()?;
                let count = pair.get(1)?.as_int()?;
                (start >= 0 && count >= 0).then_some((start as u64, count as u64))
            })
            .collect(),
        None => vec![(0, size)],
    };
    let mut entries: Vec<(u32, XrefEntry)> = Vec::new();
    let mut pos = 0usize;
    'subsections: for (start, count) in subsections {
        for i in 0..count {
            if pos + entry_len > decoded.len() {
                break 'subsections; // lenient: truncated data ends the table
            }
            let kind = if w1 == 0 {
                1
            } else {
                read_be(&decoded[pos..pos + w1])
            };
            let field2 = read_be(&decoded[pos + w1..pos + w1 + w2]);
            let field3 = read_be(&decoded[pos + w1 + w2..pos + entry_len]);
            pos += entry_len;
            let entry = match kind {
                1 => XrefEntry::InFile {
                    offset: field2,
                    gen: field3.min(65535) as u16,
                },
                2 => match (u32::try_from(field2), u32::try_from(field3)) {
                    (Ok(stream_num), Ok(index)) => XrefEntry::InStream { stream_num, index },
                    _ => XrefEntry::Free,
                },
                // Type 0 is free; unknown types read as references to the
                // null object, which a free entry models exactly.
                _ => XrefEntry::Free,
            };
            if let Ok(num) = u32::try_from(start + i) {
                entries.push((num, entry));
            }
        }
    }
    let prev = dict.get_int("Prev").and_then(non_negative);
    let span = Span {
        start: base,
        end: base + parser.pos() as u64,
    };
    let entry_count = entries.len();
    Ok(Some(ParsedSection {
        record: SectionRecord {
            kind: XrefKind::Stream,
            span,
            entries: entry_count,
            trailer_dict: dict,
            trailer_span: span,
        },
        entries,
        prev,
        xrefstm: None,
    }))
}

/// Big-endian integer from up to 8 bytes; an empty slice reads as 0.
fn read_be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |acc, &b| (acc << 8) | u64::from(b))
}

/// Keeps non-negative integers as offsets, dropping the rest.
fn non_negative(value: i64) -> Option<u64> {
    u64::try_from(value).ok()
}

/// Merged cross-reference entries plus the merged trailer, mirroring the
/// sync loader's newest-wins semantics.
pub(crate) struct XrefIndex {
    /// Consumed by `get_object`/`resolve` (Plan 02 task 8).
    #[allow(dead_code)]
    pub(crate) entries: HashMap<u32, XrefEntry>,
    /// Consumed, with `trailer_span` below, by a `merged_trailer()`
    /// accessor (Plan 02 task 11).
    #[allow(dead_code)]
    pub(crate) trailer: Dict,
    /// Span for the single merged `Trailer` element: the newest section's
    /// trailer region (classic), or that section's own span (stream) —
    /// adopted rule 4.
    #[allow(dead_code)] // see `trailer` above (Plan 02 task 11)
    pub(crate) trailer_span: Span,
}

/// A PDF document over an async random-access backend. The whole file is
/// never read: opening fetches only the tail, the xref chain and the page
/// tree; objects are fetched span-by-span on demand.
///
/// Cloning is cheap (a shared handle); every method takes `&self`, so one
/// instance can serve many tasks concurrently.
#[derive(Clone)]
pub struct AsyncDocument {
    pub(crate) inner: Arc<DocumentInner>,
}

pub(crate) struct DocumentInner {
    /// Read through `fetcher()`; consumed by `get_object`/`resolve` (Plan 02
    /// task 8).
    #[allow(dead_code)]
    pub(crate) backend: Arc<dyn Backend>,
    /// Read through `fetcher()`; consumed by `get_object`/`resolve` (Plan 02
    /// task 8).
    #[allow(dead_code)]
    pub(crate) file_len: u64,
    pub(crate) version: (u8, u8),
    /// Span of the `%PDF-` header run; `None` when the first 1 KiB holds
    /// no header (the Header element is then omitted, adopted rule 1).
    /// Exposed by a `header_span()` accessor consumed by the element
    /// stream's physical layer (Plan 02 task 11).
    #[allow(dead_code)]
    pub(crate) header_span: Option<Span>,
    /// Consumed by `get_object`/`resolve` reading `xref.entries` (Plan 02
    /// task 8).
    #[allow(dead_code)]
    pub(crate) xref: XrefIndex,
    /// Sections in chain order — newest→oldest — for the element stream.
    /// Exposed by a `sections()` accessor consumed by the element stream's
    /// physical layer (Plan 02 task 11).
    #[allow(dead_code)]
    pub(crate) sections: Vec<SectionRecord>,
    /// Exposed by a `startxref_record()` accessor consumed by the element
    /// stream's physical layer (Plan 02 task 11).
    #[allow(dead_code)]
    pub(crate) startxref: StartXrefRecord,
    /// Exposed by an `eof_span()` accessor consumed by the element stream's
    /// physical layer (Plan 02 task 11).
    #[allow(dead_code)]
    pub(crate) eof_span: Option<Span>,
}

impl AsyncDocument {
    /// Opens a file through a [`FileBackend`] wrapped in a
    /// [`CachedBackend`] with default capacity.
    pub async fn open(path: impl AsRef<Path>) -> Result<AsyncDocument> {
        let backend = FileBackend::open(path).await.map_err(Error::from)?;
        AsyncDocument::from_arc(Arc::new(CachedBackend::new(backend))).await
    }

    /// Opens an in-memory document through an uncached [`MemBackend`].
    pub async fn from_bytes(bytes: impl Into<Bytes>) -> Result<AsyncDocument> {
        AsyncDocument::from_arc(Arc::new(MemBackend::from(bytes.into()))).await
    }

    /// Opens a document over any backend, as-is (no cache is added).
    pub async fn with_backend(backend: impl Backend) -> Result<AsyncDocument> {
        AsyncDocument::from_arc(Arc::new(backend)).await
    }

    /// The open flow: header window → tail scan → xref chain → indexes.
    async fn from_arc(backend: Arc<dyn Backend>) -> Result<AsyncDocument> {
        let file_len = backend.len().await.map_err(Error::from)?;
        let fetcher = Fetcher {
            backend: Arc::clone(&backend),
            len: file_len,
        };
        let head = fetcher.window(0, 1024).await?;
        let version = parse_version(&head);
        let header_span = header_span_in(&head);
        let (startxref, eof_span) = find_tail(&fetcher).await?;
        let (xref, sections) = load_xref_chain(&fetcher, startxref.offset).await?;
        let inner = DocumentInner {
            backend,
            file_len,
            version,
            header_span,
            xref,
            sections,
            startxref,
            eof_span,
        };
        Ok(AsyncDocument {
            inner: Arc::new(inner),
        })
    }

    /// The PDF version from the header, e.g. `(1, 7)`.
    pub fn version(&self) -> (u8, u8) {
        self.inner.version
    }

    /// A fetch helper bound to this document's backend.
    #[allow(dead_code)] // consumed by get_object/resolve (Plan 02 task 8)
    pub(crate) fn fetcher(&self) -> Fetcher {
        Fetcher {
            backend: Arc::clone(&self.inner.backend),
            len: self.inner.file_len,
        }
    }
}

/// Initial window for an xref section, doubling until the section parses
/// completely.
const SECTION_WINDOW: usize = 4096;

/// Fetches and parses the section at `offset` through a growing window.
async fn parse_section_at(fetcher: &Fetcher, offset: u64) -> Result<ParsedSection> {
    let mut window = SECTION_WINDOW;
    loop {
        let buf = fetcher.window(offset, window).await?;
        let at_eof = offset + buf.len() as u64 >= fetcher.len;
        if let Some(parsed) = parse_section_window(&buf, offset, fetcher.len, at_eof)? {
            return Ok(parsed);
        }
        // None: the window ended inside the section — double and refetch.
        window = window.saturating_mul(2);
    }
}

/// Walks the section chain newest→oldest starting at `start`, merging every
/// section into one index (first-seen entries and trailer keys win). A
/// classic trailer's `/XRefStm` section (hybrid file, ISO 32000 §7.5.8.4)
/// merges ahead of its table — the table marks the stream's objects free to
/// hide them from readers without stream support — and both merge before
/// `/Prev` is followed. Visited offsets guard against loops. Merge order and
/// emission order are independent: entries still merge hybrid-before-table
/// (so the hybrid's objects win over the table's masking free entries), but
/// sections are *emitted* classic-table-before-its-hybrid-stream, matching
/// pdfboss-core's element iterator — the parity arbiter — which yields
/// `[Table, Stream]` for a hybrid file (see
/// `pdfboss_core::elements::tests::hybrid_xrefstm_yields_both_sections`).
/// Beyond a hybrid pair, sections come back in chain order — newest→oldest
/// — for the element stream. The merged trailer's span is the startxref
/// section's trailer region.
pub(crate) async fn load_xref_chain(
    fetcher: &Fetcher,
    start: u64,
) -> Result<(XrefIndex, Vec<SectionRecord>)> {
    let mut entries: HashMap<u32, XrefEntry> = HashMap::new();
    let mut trailer = Dict::new();
    let mut trailer_span: Option<Span> = None;
    let mut sections: Vec<SectionRecord> = Vec::new();
    let mut visited: HashSet<u64> = HashSet::new();
    let mut next = Some(start);
    while let Some(offset) = next {
        if !visited.insert(offset) {
            break;
        }
        let parsed = parse_section_at(fetcher, offset).await?;
        if trailer_span.is_none() {
            trailer_span = Some(parsed.record.trailer_span);
        }
        // Merge (not emit) the hybrid ahead of its table: first-seen-wins
        // means the hybrid's objects must beat the table's masking free
        // entries. Its record is held back and pushed after the table's
        // below, so emission order stays classic-then-hybrid.
        let mut hybrid_record = None;
        if let Some(hybrid_offset) = parsed.xrefstm.filter(|&v| v < fetcher.len) {
            if visited.insert(hybrid_offset) {
                // Lenient: a broken hybrid stream leaves the table alone.
                if let Ok(hybrid) = parse_section_at(fetcher, hybrid_offset).await {
                    merge_section(&mut entries, &mut trailer, &hybrid);
                    hybrid_record = Some(hybrid.record);
                }
            }
        }
        next = parsed.prev.filter(|&v| v < fetcher.len);
        merge_section(&mut entries, &mut trailer, &parsed);
        sections.push(parsed.record);
        if let Some(record) = hybrid_record {
            sections.push(record);
        }
    }
    if entries.is_empty() {
        return Err(Error::Core(pdfboss_core::Error::InvalidXref));
    }
    // Non-empty entries imply at least one parsed section, which set the
    // span before any merge could run.
    let trailer_span = trailer_span.expect("set on the first parsed section");
    Ok((
        XrefIndex {
            entries,
            trailer,
            trailer_span,
        },
        sections,
    ))
}

/// Merges a section into the accumulated index: entries and trailer keys
/// already present win (sections are walked newest to oldest).
fn merge_section(
    entries: &mut HashMap<u32, XrefEntry>,
    trailer: &mut Dict,
    parsed: &ParsedSection,
) {
    for (num, entry) in &parsed.entries {
        entries.entry(*num).or_insert(*entry);
    }
    for (key, value) in parsed.record.trailer_dict.iter() {
        if trailer.get(&key.0).is_none() {
            trailer.insert(key.clone(), value.clone());
        }
    }
}

/// Compile-time guarantee that documents can be shared across tasks.
#[allow(dead_code)]
fn assert_document_is_shareable()
where
    AsyncDocument: Send + Sync + Clone,
{
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

    use pdfboss_core::xref::XrefEntry;

    /// Extracts the section bytes starting at the classic `xref` keyword
    /// (through end of file) plus the section's absolute offset.
    fn classic_section(data: &[u8]) -> (Vec<u8>, u64) {
        let off = pos_of(data, b"xref\n0 ");
        (data[off..].to_vec(), off as u64)
    }

    #[test]
    fn classic_section_window_parses_entries_and_trailer() {
        let data = pdfboss_testkit::simple_doc("sections");
        let (buf, base) = classic_section(&data);
        let file_len = data.len() as u64;
        let parsed = parse_section_window(&buf, base, file_len, true)
            .unwrap()
            .expect("complete section parses");
        assert_eq!(parsed.record.kind, pdfboss_core::elements::XrefKind::Table);
        assert_eq!(parsed.record.entries, 6); // objects 0..=5
        assert_eq!(parsed.entries.len(), 6);
        assert!(matches!(
            parsed.entries.iter().find(|(num, _)| *num == 0),
            Some((0, XrefEntry::Free))
        ));
        let obj1_off = pos_of(&data, b"1 0 obj") as u64;
        assert!(parsed.entries.iter().any(|(num, entry)| *num == 1
            && matches!(entry, XrefEntry::InFile { offset, gen: 0 } if *offset == obj1_off)));
        assert_eq!(
            parsed.record.trailer_dict.get_ref("Root").map(|r| r.num),
            Some(1)
        );
        assert_eq!(parsed.prev, None);
        assert_eq!(parsed.xrefstm, None);
        // Spans: section runs from the xref keyword to the trailer keyword;
        // the trailer span covers `trailer << … >>`.
        assert_eq!(parsed.record.span.start, base);
        let trailer_off = pos_of(&data, b"trailer") as u64;
        assert_eq!(parsed.record.span.end, trailer_off);
        assert_eq!(parsed.record.trailer_span.start, trailer_off);
        let dict_end = pos_of(&data, b"startxref") as u64;
        assert!(parsed.record.trailer_span.end > trailer_off);
        assert!(parsed.record.trailer_span.end <= dict_end);
    }

    #[test]
    fn truncated_classic_section_asks_for_more_bytes() {
        let data = pdfboss_testkit::simple_doc("cut short");
        let (buf, base) = classic_section(&data);
        let file_len = data.len() as u64;
        // Cut mid-table: with more file remaining the parser must ask for a
        // wider window instead of failing or silently succeeding.
        let cut = &buf[..40];
        assert!(parse_section_window(cut, base, file_len, false)
            .unwrap()
            .is_none());
        // The same truncated bytes at real end of file are a hard error.
        assert!(parse_section_window(cut, base, base + 40, true).is_err());
    }

    #[test]
    fn xref_stream_section_window_parses_entries() {
        let (dict, payload) = pdfboss_testkit::objstm_payload(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
        ]);
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.stream(6, &dict, &payload);
        let data = b.build_xref_stream(1);
        let off = pos_of(&data, b"7 0 obj") as u64; // the xref stream object
        let buf = data[off as usize..].to_vec();
        let parsed = parse_section_window(&buf, off, data.len() as u64, true)
            .unwrap()
            .expect("complete stream section parses");
        assert_eq!(parsed.record.kind, pdfboss_core::elements::XrefKind::Stream);
        assert_eq!(parsed.record.span.start, off);
        assert_eq!(parsed.record.trailer_span, parsed.record.span);
        assert!(parsed.entries.iter().any(|(num, entry)| *num == 1
            && matches!(
                entry,
                XrefEntry::InStream {
                    stream_num: 6,
                    index: 0
                }
            )));
        assert!(parsed
            .entries
            .iter()
            .any(|(num, entry)| *num == 6 && matches!(entry, XrefEntry::InFile { .. })));
        assert_eq!(
            parsed
                .record
                .trailer_dict
                .get_name("Type")
                .map(|n| n.0.as_str()),
            Some("XRef")
        );
    }

    #[test]
    fn implausible_subsection_count_is_a_hard_error() {
        // A count no file of this length could hold must fail immediately,
        // not grow the window forever.
        let buf = b"xref\n0 999999999\n".to_vec();
        assert!(parse_section_window(&buf, 0, 4096, false).is_err());
    }

    #[test]
    fn truncated_stream_section_asks_for_more_bytes() {
        // Reviewer's minimal reproducer: a window cut mid-dictionary, before
        // the `stream` keyword is even reached, leniently parses as a plain
        // `Object::Dict` (core's dict parser breaks on `Eof`) — that must
        // ask for a wider window, not hard-error.
        let mid_dict = b"7 0 obj\n<< /Type /XRef /Length 10 ".to_vec();
        assert!(parse_section_window(&mid_dict, 0, 10_000, false)
            .unwrap()
            .is_none());
        // The same truncated bytes at real end of file are a hard error.
        assert!(parse_section_window(&mid_dict, 0, mid_dict.len() as u64, true).is_err());

        // A real stream section cut partway into its (still-encoded) stream
        // data must also ask for a wider window.
        let (dict, payload) = pdfboss_testkit::objstm_payload(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
        ]);
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.stream(6, &dict, &payload);
        let data = b.build_xref_stream(1);
        let off = pos_of(&data, b"7 0 obj");
        let buf = data[off..].to_vec();
        let base = off as u64;
        let file_len = data.len() as u64;
        let stream_kw = find_bytes(&buf, b"stream\n").expect("stream keyword present");
        // Cut a few bytes into the stream data: past the keyword, well
        // short of `endstream`.
        let cut = &buf[..stream_kw + b"stream\n".len() + 4];
        assert!(parse_section_window(cut, base, file_len, false)
            .unwrap()
            .is_none());
    }

    use pdfboss_core::xref::load_xref;

    /// Asserts the async xref agrees with the sync loader entry-for-entry
    /// for object numbers 0..size.
    async fn assert_xref_parity(data: Vec<u8>) {
        let sync_xref = load_xref(&data).unwrap();
        let size = sync_xref.trailer.get_int("Size").unwrap_or(64).max(1) as u32;
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        for num in 0..size + 2 {
            assert_eq!(
                doc.inner.xref.entries.get(&num).copied(),
                sync_xref.get(num),
                "entry for object {num}"
            );
        }
        assert_eq!(
            doc.inner.xref.trailer.get_ref("Root"),
            sync_xref.trailer.get_ref("Root")
        );
        assert_eq!(
            doc.inner.xref.trailer.get_int("Size"),
            sync_xref.trailer.get_int("Size")
        );
    }

    #[tokio::test]
    async fn classic_document_matches_sync_xref() {
        assert_xref_parity(simple_doc("chain walk")).await;
        let doc = AsyncDocument::from_bytes(simple_doc("chain walk"))
            .await
            .unwrap();
        assert_eq!(doc.version(), (1, 7));
        assert_eq!(doc.inner.sections.len(), 1);
        let clone = doc.clone();
        assert_eq!(clone.version(), (1, 7));
    }

    #[tokio::test]
    async fn xref_stream_document_matches_sync_xref() {
        let (dict, payload) = pdfboss_testkit::objstm_payload(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
        ]);
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.stream(6, &dict, &payload);
        assert_xref_parity(b.build_xref_stream(1)).await;
    }

    /// An incremental update: a classic base section, then an xref stream
    /// whose /Prev points back at it.
    fn prev_chain_doc() -> Vec<u8> {
        let mut data = b"%PDF-1.5\n".to_vec();
        let obj1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let obj2_old = data.len();
        data.extend_from_slice(b"2 0 obj\n(old)\nendobj\n");
        let classic_off = data.len();
        data.extend_from_slice(b"xref\n0 3\n0000000000 65535 f\r\n");
        data.extend_from_slice(format!("{obj1:010} 00000 n\r\n").as_bytes());
        data.extend_from_slice(format!("{obj2_old:010} 00000 n\r\n").as_bytes());
        data.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n");
        let obj2_new = data.len();
        data.extend_from_slice(b"2 0 obj\n(new)\nendobj\n");
        let stream_off = data.len();
        let mut fields = Vec::new();
        for offset in [obj2_new, stream_off] {
            fields.push(1u8);
            fields.extend_from_slice(&(offset as u32).to_be_bytes());
            fields.extend_from_slice(&0u16.to_be_bytes());
        }
        data.extend_from_slice(
            format!(
                "4 0 obj\n<< /Type /XRef /Size 5 /W [1 4 2] /Index [2 1 4 1] \
                 /Prev {} /Root 1 0 R /Length {} >>\nstream\n",
                classic_off,
                fields.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&fields);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        data.extend_from_slice(format!("startxref\n{stream_off}\n%%EOF\n").as_bytes());
        data
    }

    #[tokio::test]
    async fn prev_chain_merges_newest_wins() {
        let data = prev_chain_doc();
        let obj2_new = pos_of(&data, b"2 0 obj\n(new)") as u64;
        assert_xref_parity(data.clone()).await;
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        assert!(matches!(
            doc.inner.xref.entries.get(&2),
            Some(XrefEntry::InFile { offset, .. }) if *offset == obj2_new
        ));
        // Two sections, in chain order: the startxref section (the xref
        // stream) first, then the /Prev classic table (adopted rule 4).
        assert_eq!(doc.inner.sections.len(), 2);
        assert!(doc.inner.sections[0].span.start > doc.inner.sections[1].span.start);
        assert_eq!(
            doc.inner.sections[0].kind,
            pdfboss_core::elements::XrefKind::Stream
        );
        assert_eq!(
            doc.inner.sections[1].kind,
            pdfboss_core::elements::XrefKind::Table
        );
        // The merged trailer's span is the newest (stream) section's own
        // span — stream sections have no separate trailer region.
        assert_eq!(doc.inner.xref.trailer_span, doc.inner.sections[0].span);
    }

    /// A hybrid file: the classic table hides object 2 behind a free entry
    /// while /XRefStm reveals it.
    fn hybrid_doc() -> Vec<u8> {
        let mut data = b"%PDF-1.5\n".to_vec();
        let obj1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let obj2 = data.len();
        data.extend_from_slice(b"2 0 obj\n(hidden)\nendobj\n");
        let stm_off = data.len();
        let mut fields = Vec::new();
        for offset in [obj2, stm_off] {
            fields.push(1u8);
            fields.extend_from_slice(&(offset as u32).to_be_bytes());
            fields.extend_from_slice(&0u16.to_be_bytes());
        }
        data.extend_from_slice(
            format!(
                "3 0 obj\n<< /Type /XRef /Size 4 /W [1 4 2] /Index [2 1 3 1] \
                 /Root 1 0 R /Length {} >>\nstream\n",
                fields.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&fields);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        let classic_off = data.len();
        data.extend_from_slice(b"xref\n0 3\n0000000000 65535 f\r\n");
        data.extend_from_slice(format!("{obj1:010} 00000 n\r\n").as_bytes());
        data.extend_from_slice(b"0000000000 00001 f\r\n");
        data.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R /XRefStm {stm_off} >>\n").as_bytes(),
        );
        data.extend_from_slice(format!("startxref\n{classic_off}\n%%EOF\n").as_bytes());
        data
    }

    #[tokio::test]
    async fn hybrid_xrefstm_beats_the_tables_free_entry() {
        let data = hybrid_doc();
        let obj2 = pos_of(&data, b"2 0 obj\n(hidden)") as u64;
        assert_xref_parity(data.clone()).await;
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        assert!(matches!(
            doc.inner.xref.entries.get(&2),
            Some(XrefEntry::InFile { offset, .. }) if *offset == obj2
        ));
        // Emission order must match pdfboss-core's element iterator (the
        // parity arbiter): the classic section first, then its hybrid
        // /XRefStm section — even though the hybrid's entries merge ahead
        // of the classic table's masking free entries (asserted above).
        let kinds: Vec<pdfboss_core::elements::XrefKind> =
            doc.inner.sections.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                pdfboss_core::elements::XrefKind::Table,
                pdfboss_core::elements::XrefKind::Stream
            ],
            "classic section first, then its hybrid /XRefStm section"
        );
    }

    #[tokio::test]
    async fn open_reads_from_disk() {
        let path =
            std::env::temp_dir().join(format!("pdfboss-aio-doc-test-{}.pdf", std::process::id()));
        std::fs::write(&path, simple_doc("from disk")).unwrap();
        let doc = AsyncDocument::open(&path).await.unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(doc.version(), (1, 7));
    }
}
