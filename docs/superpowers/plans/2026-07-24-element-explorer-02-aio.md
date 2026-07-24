# pdfboss-aio Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new `pdfboss-aio` crate providing fully async, range-fetching PDF access — `AsyncDocument` opens huge local files and remote HTTP PDFs by fetching only the byte spans it needs (tail, xref chain, page tree, individual objects), never the whole file.

**Architecture:** `pdfboss-aio` is built sans-I/O style on the existing sync parser: an object-safe `Backend` trait (boxed futures) abstracts random-access byte sources (memory, positioned file reads, HTTP ranges), a chunked LRU `CachedBackend` sits over slow backends, and `AsyncDocument` fetches small growing windows that it hands to `pdfboss-core`'s synchronous `Lexer`/`Parser`/filters. `ElementStream` mirrors core's sync `Elements` iterator (from plan 01) as a `futures_core::Stream`, with identical ordering and salvage semantics.

**Tech Stack:** Rust (edition 2021), tokio 1 (rt, rt-multi-thread, fs, sync, io-util), futures-core/futures-util 0.3, bytes 1, thiserror 2, reqwest 0.12 (rustls-tls, behind the `http` feature), pdfboss-core, pdfboss-testkit (dev).

**Prerequisite:** Plan 01 (`2026-07-24-element-explorer-01-core.md`) must be merged first: this plan consumes `pdfboss_core::elements::{Span, Element, ElementOpts, XrefKind}` and `Document::elements` exactly as pinned in the spec (`docs/superpowers/specs/2026-07-24-pdf-element-explorer-design.md`), plus core's existing public API. Tasks 1–4 compile without plan 01; Task 5 onward requires it.

## Global Constraints

- **Cleanroom rule:** everything is implemented purely from ISO 32000. NEVER name any other PDF library anywhere — code, comments, docs, tests, commit messages, plan prose. Non-PDF dependencies (tokio, reqwest, futures, bytes, thiserror) are fine.
- **`pdfboss-core` gains zero new dependencies.** No async, no serde anywhere in core. This plan does not touch core at all.
- **The existing sync API** (`Document`, `Page`, text, render) **and all existing tests stay untouched.** New capability is additive.
- **Never create underscore-prefixed identifiers** — no `_foo` variables, fields, methods, or parameters, even where surrounding code does it. Use full names; restructure code instead of discarding values into named underscore bindings.
- **Edition 2021**, `cargo fmt` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean.
- **Shared build cache:** every build/test command uses the global shared target dir: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target`. Never per-agent target dirs.

## Contract consumed (from the spec, plan 01, and existing core)

From plan 01 (`pdfboss_core::elements`):

```rust
pub struct Span { pub start: u64, pub end: u64 }   // Clone, Copy, PartialEq, Eq, Debug
pub enum Element {
    Header { version: (u8, u8), span: Span },
    IndirectObject { r: ObjRef, object: Object, span: Span, in_objstm: Option<(ObjRef, Span)> },
    XrefSection { kind: XrefKind, span: Span, entries: usize },
    Trailer { dict: Dict, span: Span },
    StartXref { offset: u64, span: Span },
    Eof { span: Span },
    Page { index: usize, r: ObjRef },
    Font { page: Option<usize>, r: ObjRef, subtype: Name, base_font: Option<Name> },
    Image { page: Option<usize>, r: ObjRef, width: u32, height: u32 },
    Annotation { page: usize, r: ObjRef, subtype: Name },
    ContentOp { page: usize, op: content::Op, span_in_content: Span },
}                                                   // Clone, Debug
pub enum XrefKind { Table, Stream }                 // Clone, Copy, PartialEq, Eq, Debug
pub struct ElementOpts { pub physical: bool, pub logical: bool,
                         pub pages: Option<Vec<usize>>, pub content_ops: bool }
// Default = physical: true, logical: true, pages: None, content_ops: false
impl Document { pub fn elements(&self, opts: ElementOpts) -> Elements<'_>; }
impl<'a> Iterator for Elements<'a> { type Item = pdfboss_core::Result<Element>; }
```

From existing core (signatures verified in the sources):

```rust
// pdfboss_core (lib.rs re-exports)
pub use document::{Document, Metadata, Page};
pub use error::{Error, Result};
pub use object::{Dict, Name, ObjRef, Object, Stream};

// pdfboss_core::lexer
impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self;
    pub fn at(data: &'a [u8], pos: usize) -> Self;
    pub fn pos(&self) -> usize;
    pub fn seek(&mut self, pos: usize);
    pub fn next_token(&mut self) -> Result<Token>;
    pub fn peek_token(&mut self) -> Result<Token>;
    pub fn skip_whitespace_and_comments(&mut self);
    pub fn data(&self) -> &'a [u8];
}
// Token variants used: Int(i64), Real(f64), Name(Name), LitString/HexString(Vec<u8>),
// ArrayOpen, ArrayClose, DictOpen, DictClose, Keyword(Vec<u8>), Eof

// pdfboss_core::parser
pub trait Resolve { fn resolve_ref(&self, r: ObjRef) -> Option<Object>; }
pub struct NoResolve;
impl<'a> Parser<'a> {
    pub fn at(data: &'a [u8], pos: usize) -> Self;
    pub fn pos(&self) -> usize;
    pub fn parse_object(&mut self, resolver: &dyn Resolve) -> Result<Object>;
    pub fn parse_indirect(&mut self, resolver: &dyn Resolve) -> Result<(ObjRef, Object)>;
}

// pdfboss_core::xref
pub enum XrefEntry { Free, InFile { offset: u64, gen: u16 },
                     InStream { stream_num: u32, index: u32 } }
pub struct Xref { pub trailer: Dict, /* private map */ }
impl Xref { pub fn get(&self, num: u32) -> Option<XrefEntry>; }
pub fn load_xref(data: &[u8]) -> Result<Xref>;      // used in tests for parity only

// pdfboss_core::filters
pub fn decode_stream(stream: &Stream, resolver: &dyn Resolve) -> Result<Vec<u8>>;

// pdfboss_core::content
pub fn parse_content(data: &[u8]) -> Result<Vec<Op>>;

// pdfboss_core::object
pub fn decode_text_string(bytes: &[u8]) -> String;

// pdfboss_testkit
pub fn simple_doc(text: &str) -> Vec<u8>;
pub fn multi_page_doc(pages: &[&str]) -> Vec<u8>;
pub fn objstm_payload(objects: &[(u32, &str)]) -> (String, Vec<u8>);
pub struct PdfBuilder;   // new(), version(u8,u8), trailer_extra(&str),
                         // object(u32,&str), stream(u32,&str,&[u8]),
                         // build(u32) -> Vec<u8>, build_xref_stream(u32) -> Vec<u8>
```

## Adopted span & ordering rules

The spec pins ordering categories but not every byte boundary. These concrete rules are adopted here and MUST match plan 01's core iterator (the parity tests in Task 13 are the arbiter — if plan 01 chose differently, plan 01's choice wins and the aio side is aligned to it, because core is the reference implementation):

1. **Header element and span** (pinned by plan 01): a `Header` element is yielded ONLY when `%PDF-` is found in the first 1 KiB. Its span runs from the match start through the run of version characters (ASCII digits and dots) after `%PDF-` — e.g. `%PDF-1.7` at offset 0 → `Span { start: 0, end: 8 }`. No header in the first 1 KiB means no `Header` element (the version still defaults to 1.4).
2. **In-file object span** = xref offset `..` parser position after `endobj`. A window parse is accepted only when at least 16 bytes of slack remain after the parse end (or the window reaches end of file), so trailing-keyword lookahead is never cut mid-token.
3. **Object-stream members** follow their container's own `IndirectObject` element immediately, ordered by in-stream index. Member `span` = the container's span; `in_objstm = Some((container_ref, member_range))` where the member range within the decoded stream runs from `first + offset[i]` to the parser position after parsing the member.
4. **Sections in chain order, one merged Trailer** (pinned by plan 01): `XrefSection` elements are emitted in chain order — newest→oldest: the startxref section first, then each `/Prev`; a hybrid `/XRefStm` section appears where the walk visits it (directly before the classic section that references it). **XrefSection span:** classic = section offset `..` start of the `trailer` keyword; stream = the xref-stream object's full span. After all sections comes exactly ONE `Trailer` element whose dict is the MERGED trailer dictionary and whose span is the NEWEST section's trailer region (classic: `trailer` keyword `..` byte after the dictionary; stream sections have no separate trailer region, so that section's own span is used). Tail shape for a single-section classic document: `xref`, `trailer`, `startxref`, `eof`.
5. **One `StartXref` and one `Eof` element** — the last in the file, found by the tail scan. `StartXref` span = `startxref` keyword through the offset integer. `Eof` span = the 5 bytes of `%%EOF`; omitted when no `%%EOF` exists.
6. **`page_count()`** returns the flattened page-tree length (the async document always flattens at open — this mirrors the sync document's post-flatten authoritative count, not its declared-`/Count` shortcut).
7. **Logical layer:** per page — fonts (from inherited `/Resources` `/Font`, sorted by resource key name), then images (`/XObject` entries whose resolved `/Subtype` is `Image`, sorted by resource key name), then annotations (`/Annots` array order), then content ops. Only entries that are indirect references yield elements (an `ObjRef` is required). A font or annotation with a missing `/Subtype` still yields its element with `Name(String::new())` (lenient, pinned by plan 01); images legitimately require `/Subtype` = `Image` to qualify at all. Image width/height default to 0 when missing or invalid.
8. **ContentOp `span_in_content`** = first byte of the op's first operand token `..` byte after its operator keyword (inline images: through `EI`). Operators core's `parse_content` skips as unknown yield no element; a content lexer error yields one `Err` item and ends that page's ops.
9. **No whole-file recovery scan** in aio (it would violate the never-read-the-whole-file guarantee): an unusable xref chain yields `Error::Core(pdfboss_core::Error::InvalidXref)`.
10. **Release-please:** no config/manifest change. All workspace crates share the root-managed workspace version (`release-please-config.json` package `"."` with `extra-files: ["Cargo.toml"]` and the `# x-release-please-version` marker in the root `Cargo.toml`). `pdfboss-aio` uses `version.workspace = true` exactly like `pdfboss-core`; registering it separately would double-version it. It is publishable (no `publish = false`).
11. **HTTP transport errors** cross the `io::Result` boundary of the `Backend` trait via an internal marker payload inside `std::io::Error`, recovered by `From<std::io::Error> for Error` into `Error::RangeUnsupported` / `Error::Http`.

---

### Task 1: Crate scaffolding and error.rs

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/Cargo.toml`
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/Cargo.toml`
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/error.rs`
- Test: unit tests inside `src/error.rs`

**Interfaces:**
- Consumes: `pdfboss_core::Error` (variants listed in the contract section).
- Produces (relied on by every later task and plans 03/04/05):

```rust
pub type Result<T> = std::result::Result<T, Error>;
pub enum Error {
    Core(pdfboss_core::Error),
    Io(std::io::Error),
    #[cfg(feature = "http")]
    Http { status: Option<u16>, msg: String },
    RangeUnsupported,
    TruncatedRead { offset: u64, wanted: usize, got: usize },
}
impl From<pdfboss_core::Error> for Error;
impl From<std::io::Error> for Error;   // recovers transport markers, see Task 14
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Register the crate and write the test module first. In `/Users/mohamed.tahrioui/private/pdfboss/Cargo.toml`, the changed items become (full new versions of both tables):

```toml
[workspace]
resolver = "2"
members = [
    "crates/pdfboss-core",
    "crates/pdfboss-text",
    "crates/pdfboss-encoding",
    "crates/pdfboss-render",
    "crates/pdfboss-aio",
    "crates/pdfboss-cli",
    "crates/pdfboss-py",
    "crates/pdfboss-testkit",
]
```

```toml
[workspace.dependencies]
criterion = { version = "0.5", default-features = false, features = ["cargo_bench_support"] }
tokio = { version = "1", default-features = false, features = ["rt", "rt-multi-thread", "fs", "sync", "io-util"] }
futures-core = "0.3"
futures-util = { version = "0.3", default-features = false, features = ["std"] }
bytes = "1"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
```

Create `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/Cargo.toml`:

```toml
[package]
name = "pdfboss-aio"
description = "Async, range-fetching PDF access for pdfboss: huge files, many documents, remote HTTP sources (ISO 32000)"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
pdfboss-core = { path = "../pdfboss-core" }
tokio = { workspace = true }
futures-core = { workspace = true }
futures-util = { workspace = true }
bytes = { workspace = true }
thiserror = "2"
reqwest = { workspace = true, optional = true }

[features]
# Remote documents over HTTP range requests.
http = ["dep:reqwest"]

[dev-dependencies]
pdfboss-testkit = { path = "../pdfboss-testkit" }
tokio = { workspace = true, features = ["macros", "net", "time"] }
```

Create `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`:

```rust
//! Async, range-fetching PDF access for pdfboss: open huge files without
//! loading them, hold many documents concurrently, and read remote PDFs
//! over HTTP range requests. Built sans-I/O style on the synchronous
//! pdfboss-core machinery: bytes are fetched in small windows and handed
//! to the existing sync lexer, parser and filters. The whole file is
//! never read.

pub mod error;

pub use error::{Error, Result};
```

Create `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/error.rs` containing ONLY the module doc and the test module for now:

```rust
//! Error type for pdfboss-aio: wraps core parse errors and transport
//! failures, with dedicated variants for range-refusing HTTP servers and
//! short reads. Messages are prefixed by layer ("parse:", "i/o:", "http:")
//! so downstream consumers can present them uniformly.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_core_and_io_errors_with_layer_prefixes() {
        let core = Error::from(pdfboss_core::Error::InvalidXref);
        assert!(matches!(core, Error::Core(pdfboss_core::Error::InvalidXref)));
        assert_eq!(
            core.to_string(),
            "parse: invalid or unrecoverable cross-reference data"
        );
        let io = Error::from(std::io::Error::other("boom"));
        assert!(matches!(io, Error::Io(_)));
        assert_eq!(io.to_string(), "i/o: boom");
    }

    #[test]
    fn transport_variants_render_their_context() {
        let err = Error::TruncatedRead {
            offset: 512,
            wanted: 100,
            got: 3,
        };
        assert_eq!(
            err.to_string(),
            "truncated read at offset 512: wanted 100 bytes, got 3"
        );
        assert_eq!(
            Error::RangeUnsupported.to_string(),
            "server ignored Range requests"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio error:: -- --nocapture
```

Expected failure: compile errors in `src/error.rs` — `cannot find type Error in this scope` (the enum does not exist yet).

- [ ] **Step 3: Write minimal implementation.** Insert the following between the module doc and the test module in `src/error.rs`:

```rust
/// Convenience alias used throughout pdfboss-aio.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors surfaced by pdfboss-aio.
///
/// Every fetch failure carries the offset/range it was fetching, for
/// diagnosability; parse errors wrap the core error unchanged.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A parse-layer error from the sync core machinery.
    #[error("parse: {0}")]
    Core(#[from] pdfboss_core::Error),
    /// A transport-layer I/O error.
    #[error("i/o: {0}")]
    Io(std::io::Error),
    /// An HTTP transport error (connection, status, malformed response).
    #[cfg(feature = "http")]
    #[error("http: {msg}")]
    Http { status: Option<u16>, msg: String },
    /// The server ignored `Range` requests (answered 200 with the full
    /// body instead of 206), so range-fetching cannot work.
    #[error("server ignored Range requests")]
    RangeUnsupported,
    /// A read stopped short of the requested range while more bytes were
    /// expected (the source is shorter than its declared length).
    #[error("truncated read at offset {offset}: wanted {wanted} bytes, got {got}")]
    TruncatedRead { offset: u64, wanted: usize, got: usize },
}

impl From<std::io::Error> for Error {
    fn from(inner: std::io::Error) -> Error {
        #[cfg(feature = "http")]
        if let Some(marker) = inner
            .get_ref()
            .and_then(|source| source.downcast_ref::<TransportMarker>())
        {
            return match marker {
                TransportMarker::RangeUnsupported => Error::RangeUnsupported,
                TransportMarker::Http { status, msg } => Error::Http {
                    status: *status,
                    msg: msg.clone(),
                },
            };
        }
        Error::Io(inner)
    }
}

/// Marker payload smuggled through `std::io::Error` by backends whose
/// trait methods can only return `io::Result`; recovered by
/// [`From<std::io::Error>`] above. Only the HTTP backend produces these.
#[cfg(feature = "http")]
#[derive(Debug)]
pub(crate) enum TransportMarker {
    RangeUnsupported,
    Http { status: Option<u16>, msg: String },
}

#[cfg(feature = "http")]
impl std::fmt::Display for TransportMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportMarker::RangeUnsupported => write!(f, "server ignored Range requests"),
            TransportMarker::Http { status, msg } => write!(f, "http {status:?}: {msg}"),
        }
    }
}

#[cfg(feature = "http")]
impl std::error::Error for TransportMarker {}
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio error:: -- --nocapture
```

Expected: `test error::tests::wraps_core_and_io_errors_with_layer_prefixes ... ok`, `test error::tests::transport_variants_render_their_context ... ok`; 2 passed. Also verify the http-gated code compiles:

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo check -p pdfboss-aio --all-features
```

Expected: clean check.

- [ ] **Step 5: Commit.**

```bash
git add Cargo.toml crates/pdfboss-aio && git commit -m "feat(aio): new pdfboss-aio workspace crate with layered error type"
```

---

### Task 2: backend.rs — Backend trait and MemBackend

**Files:**
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/backend.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`
- Test: unit tests inside `src/backend.rs`

**Interfaces:**
- Consumes: `futures_util::future::BoxFuture`, `bytes::Bytes`.
- Produces (relied on by Tasks 3–14 and plans 03/05):

```rust
pub use futures_util::future::BoxFuture;
pub trait Backend: Send + Sync + 'static {
    fn len(&self) -> BoxFuture<'_, std::io::Result<u64>>;
    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8])
        -> BoxFuture<'a, std::io::Result<usize>>;
}
pub struct MemBackend(/* bytes::Bytes */);
impl From<Vec<u8>> for MemBackend;
impl From<bytes::Bytes> for MemBackend;
impl Backend for MemBackend;
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Create `src/backend.rs`:

```rust
//! Random-access byte sources: in-memory bytes, positioned file reads on a
//! blocking thread, and (behind the `http` feature) remote HTTP range
//! requests. The trait is object-safe — futures are boxed — so documents
//! can hold `Arc<dyn Backend>`.

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
}
```

Add `pub mod backend;` and the re-export to `src/lib.rs` (full new file):

```rust
//! Async, range-fetching PDF access for pdfboss: open huge files without
//! loading them, hold many documents concurrently, and read remote PDFs
//! over HTTP range requests. Built sans-I/O style on the synchronous
//! pdfboss-core machinery: bytes are fetched in small windows and handed
//! to the existing sync lexer, parser and filters. The whole file is
//! never read.

pub mod backend;
pub mod error;

pub use backend::{Backend, BoxFuture, MemBackend};
pub use error::{Error, Result};
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio backend::tests -- --nocapture
```

Expected failure: compile errors — `cannot find type MemBackend`, `cannot find trait Backend`.

- [ ] **Step 3: Write minimal implementation.** Insert between the module doc and the test module in `src/backend.rs`:

```rust
use std::io;

use bytes::Bytes;
pub use futures_util::future::BoxFuture;

/// Random-access byte source. Object-safe: futures are boxed.
pub trait Backend: Send + Sync + 'static {
    /// Total length of the underlying byte source.
    fn len(&self) -> BoxFuture<'_, io::Result<u64>>;

    /// Reads up to `buf.len()` bytes at `offset` into `buf`, returning the
    /// number of bytes read. Implementations may only return a short count
    /// at end of input; anywhere else they must fill the buffer.
    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8])
        -> BoxFuture<'a, io::Result<usize>>;
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
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(data.len());
            let count = buf.len().min(data.len() - start);
            buf[..count].copy_from_slice(&data[start..start + count]);
            Ok(count)
        })
    }
}
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio backend::tests -- --nocapture
```

Expected: 3 tests pass (`mem_backend_reads_and_reports_length`, `mem_backend_short_reads_only_at_eof`, `backend_is_object_safe`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): object-safe Backend trait and MemBackend"
```

---

### Task 3: backend.rs — FileBackend

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/backend.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`
- Test: unit tests inside `src/backend.rs`

**Interfaces:**
- Consumes: `Backend`, `BoxFuture` (Task 2); `tokio::task::spawn_blocking`.
- Produces (relied on by Tasks 10, 13 and plans 03/05):

```rust
pub struct FileBackend { /* Arc<std::fs::File> + cached length */ }
impl FileBackend { pub async fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<FileBackend>; }
impl Backend for FileBackend;
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `src/backend.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio backend::tests::file_backend -- --nocapture
```

Expected failure: compile error — `cannot find struct FileBackend`.

- [ ] **Step 3: Write minimal implementation.** Add to `src/backend.rs` (below `MemBackend`'s impl, above the test module), and extend the `use` lines at the top of the file to:

```rust
use std::io;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
pub use futures_util::future::BoxFuture;
```

then add:

```rust
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
```

Update the re-export line in `src/lib.rs` to:

```rust
pub use backend::{Backend, BoxFuture, FileBackend, MemBackend};
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio backend::tests -- --nocapture
```

Expected: 5 tests pass (the 3 from Task 2 plus `file_backend_positioned_reads`, `file_backend_open_missing_file_errors`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): FileBackend with positioned reads on the blocking pool"
```

---

### Task 4: cache.rs — chunked LRU CachedBackend

**Files:**
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/cache.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`
- Test: unit tests inside `src/cache.rs`

**Interfaces:**
- Consumes: `Backend`, `BoxFuture`, `MemBackend` (Tasks 2–3).
- Produces (relied on by Tasks 10, 14 and plans 03/05):

```rust
pub const DEFAULT_CHUNK_SIZE: usize;   // 64 * 1024
pub const DEFAULT_MAX_BYTES: usize;    // 32 * 1024 * 1024
pub struct CachedBackend<B: Backend> { /* … */ }
impl<B: Backend> CachedBackend<B> {
    pub fn new(inner: B) -> Self;
    pub fn with_capacity(inner: B, chunk_size: usize, max_bytes: usize) -> Self;
}
impl<B: Backend> Backend for CachedBackend<B>;
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Create `src/cache.rs`:

```rust
//! Chunked LRU read cache over any backend: many small reads become few
//! chunk-sized fetches, and hot chunks stay resident up to a byte budget.
//! Default 64 KiB chunks, 32 MiB cap.

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
        // Spans chunks 0 and 1.
        assert_eq!(cached.read_at(30, &mut buf).await.unwrap(), 100);
        assert_eq!(buf[0], 30);
        assert_eq!(buf[99], 129);
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
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
        cached.read_at(65_000, &mut buf).await.unwrap();
        // 0 lives in chunk 0, 65_000 in chunk 1 of the 64 KiB grid: two
        // inner fetches, and a re-read of either offset adds none.
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        cached.read_at(1000, &mut buf).await.unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }
}
```

Add `pub mod cache;` to `src/lib.rs` and extend the re-exports (full new file):

```rust
//! Async, range-fetching PDF access for pdfboss: open huge files without
//! loading them, hold many documents concurrently, and read remote PDFs
//! over HTTP range requests. Built sans-I/O style on the synchronous
//! pdfboss-core machinery: bytes are fetched in small windows and handed
//! to the existing sync lexer, parser and filters. The whole file is
//! never read.

pub mod backend;
pub mod cache;
pub mod error;

pub use backend::{Backend, BoxFuture, FileBackend, MemBackend};
pub use cache::CachedBackend;
pub use error::{Error, Result};
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio cache::tests -- --nocapture
```

Expected failure: compile error — `cannot find struct CachedBackend` (and unresolved `Backend`/`BoxFuture` imports in the test module until the implementation's `use` lines exist).

- [ ] **Step 3: Write minimal implementation.** Insert between the module doc and the test module in `src/cache.rs`:

```rust
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
        state.chunks.insert(index, CacheEntry { data, stamp });
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
        Box::pin(async move {
            self.len
                .get_or_try_init(|| self.inner.len())
                .await
                .copied()
        })
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
```

Two clippy notes honored above: the eviction loop never evicts the chunk just inserted (`**candidate != index`) so a cache smaller than one chunk still serves reads, and `min_by_key` + `remove` avoids holding two mutable borrows.

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio cache::tests -- --nocapture
```

Expected: 4 tests pass (`repeated_reads_fetch_each_chunk_once`, `reads_spanning_chunks_and_eof_are_stitched`, `lru_evicts_the_coldest_chunk`, `default_capacity_uses_64_kib_chunks`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): chunked LRU CachedBackend with 64 KiB chunks and 32 MiB cap"
```

---

### Task 5: document.rs — Fetcher, tail scan, version parse

**Files:**
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/document.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`
- Test: unit tests inside `src/document.rs`

**Interfaces:**
- Consumes: `Backend` (Task 2), `Error`/`Result` (Task 1), `pdfboss_core::lexer::{Lexer, Token}`, `pdfboss_core::elements::Span` (plan 01).
- Produces (crate-internal, relied on by Tasks 6–12):

```rust
pub(crate) struct Fetcher { pub(crate) backend: std::sync::Arc<dyn Backend>, pub(crate) len: u64 }
impl Fetcher {
    pub(crate) async fn read_range(&self, start: u64, end: u64) -> Result<Vec<u8>>;
    pub(crate) async fn window(&self, offset: u64, window: usize) -> Result<Vec<u8>>;
}
pub(crate) struct StartXrefRecord { pub(crate) offset: u64, pub(crate) span: Span }
pub(crate) async fn find_tail(fetcher: &Fetcher) -> Result<(StartXrefRecord, Option<Span>)>;
pub(crate) fn parse_version(head: &[u8]) -> (u8, u8);
pub(crate) fn header_span_in(head: &[u8]) -> Option<Span>;
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Create `src/document.rs`:

```rust
//! The async document model: opening fetches only the file tail, the xref
//! chain and the page-tree nodes; objects are fetched span-by-span through
//! growing windows and parsed by the sync core machinery. The whole file
//! is never read.

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
```

Add `pub mod document;` and the re-export to `src/lib.rs` (changed lines):

```rust
pub mod backend;
pub mod cache;
pub mod document;
pub mod error;

pub use backend::{Backend, BoxFuture, FileBackend, MemBackend};
pub use cache::CachedBackend;
pub use error::{Error, Result};
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests -- --nocapture
```

Expected failure: compile errors — `cannot find struct Fetcher`, `cannot find function find_tail`, `cannot find function parse_version`.

- [ ] **Step 3: Write minimal implementation.** Insert between the module doc and the test module in `src/document.rs`:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests -- --nocapture
```

Expected: 6 tests pass (`read_range_returns_exact_bytes_and_detects_truncation`, `tail_scan_finds_startxref_and_eof`, `tail_scan_grows_past_trailing_padding`, `tail_scan_without_startxref_is_invalid_xref`, `version_parse_matches_header_and_defaults`, `header_span_covers_the_version_run`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): fetch helper, tail scan and header version parse"
```

---

### Task 6: document.rs — xref section window parsers

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/document.rs`
- Test: unit tests inside `src/document.rs`

**Interfaces:**
- Consumes: `Lexer`, `Token`, `Parser`, `NoResolve`, `pdfboss_core::filters::decode_stream`, `pdfboss_core::xref::XrefEntry`, `pdfboss_core::elements::{Span, XrefKind}`, `Dict`.
- Produces (crate-internal, relied on by Tasks 7 and 11):

```rust
pub(crate) struct SectionRecord {
    pub(crate) kind: XrefKind,
    pub(crate) span: Span,
    pub(crate) entries: usize,
    pub(crate) trailer_dict: Dict,
    pub(crate) trailer_span: Span,
}
pub(crate) struct ParsedSection {
    pub(crate) record: SectionRecord,
    pub(crate) entries: Vec<(u32, XrefEntry)>,
    pub(crate) prev: Option<u64>,
    pub(crate) xrefstm: Option<u64>,
}
/// Ok(None) = the window ended inside the section (fetch more bytes).
pub(crate) fn parse_section_window(
    buf: &[u8], base: u64, file_len: u64, at_eof: bool,
) -> Result<Option<ParsedSection>>;
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `src/document.rs`:

```rust
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
        assert!(parsed
            .entries
            .iter()
            .any(|(num, entry)| *num == 1
                && matches!(entry, XrefEntry::InFile { offset, gen: 0 } if *offset == obj1_off)));
        assert_eq!(parsed.record.trailer_dict.get_ref("Root").map(|r| r.num), Some(1));
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
        assert!(parsed
            .entries
            .iter()
            .any(|(num, entry)| *num == 1
                && matches!(entry, XrefEntry::InStream { stream_num: 6, index: 0 })));
        assert!(parsed
            .entries
            .iter()
            .any(|(num, entry)| *num == 6 && matches!(entry, XrefEntry::InFile { .. })));
        assert_eq!(
            parsed.record.trailer_dict.get_name("Type").map(|n| n.0.as_str()),
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
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests::classic_section_window -- --nocapture
```

Expected failure: compile errors — `cannot find struct SectionRecord` / `cannot find function parse_section_window`.

- [ ] **Step 3: Write minimal implementation.** Extend the `use` block at the top of `src/document.rs` to:

```rust
use std::sync::Arc;

use pdfboss_core::elements::{Span, XrefKind};
use pdfboss_core::lexer::{Lexer, Token};
use pdfboss_core::parser::{NoResolve, Parser};
use pdfboss_core::xref::XrefEntry;
use pdfboss_core::{Dict, Object};

use crate::backend::Backend;
use crate::error::{Error, Result};
```

Add below the `parse_version` helpers:

```rust
/// Bytes of slack demanded beyond a window parse end so trailing-keyword
/// lookahead (`endobj`, `endstream`, `trailer` dict close) can never be cut
/// mid-token by the window edge.
pub(crate) const PARSE_SLACK: usize = 16;

/// One cross-reference section as found while walking the chain.
#[derive(Clone)]
pub(crate) struct SectionRecord {
    pub(crate) kind: XrefKind,
    /// Classic: `xref` keyword to the `trailer` keyword. Stream: the whole
    /// xref-stream object.
    pub(crate) span: Span,
    /// Number of entries the section declares (subsection sums).
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
                        Ok(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
                        Err(_) => return incomplete(at_eof),
                    };
                    let field2 = match lexer.next_token() {
                        Ok(Token::Int(v)) if v >= 0 => v,
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
        Ok(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
        Err(_) if !at_eof => return Ok(None),
        Err(_) => return Err(Error::Core(pdfboss_core::Error::InvalidXref)),
    };
    if parser.pos() + PARSE_SLACK > buf.len() && !at_eof {
        return Ok(None); // stream data may have been cut leniently
    }
    // Trust a declared /Length only when the parsed data honors it — a
    // window cut inside the stream falls into the lenient recovery path
    // and must grow instead.
    if let Some(declared) = stream.dict.get_int("Length") {
        if declared >= 0 && stream.data.len() as u64 != declared as u64 && !at_eof {
            return Ok(None);
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
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests -- --nocapture
```

Expected: all document tests pass, including the four new ones (`classic_section_window_parses_entries_and_trailer`, `truncated_classic_section_asks_for_more_bytes`, `xref_stream_section_window_parses_entries`, `implausible_subsection_count_is_a_hard_error`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): window parsers for classic and stream xref sections"
```

---

### Task 7: document.rs — chain walk and AsyncDocument constructors

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/document.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`
- Test: unit tests inside `src/document.rs`

**Interfaces:**
- Consumes: `Fetcher`, `find_tail`, `parse_version` (Task 5), `parse_section_window`, `ParsedSection`, `SectionRecord` (Task 6), `FileBackend` (Task 3), `CachedBackend` (Task 4), `MemBackend` (Task 2).
- Produces (public API pinned by the spec; relied on by Tasks 8–14 and plans 03/04/05):

```rust
pub struct AsyncDocument { /* Arc<DocumentInner>, Clone + Send + Sync */ }
impl AsyncDocument {
    pub async fn open(path: impl AsRef<std::path::Path>) -> Result<AsyncDocument>;
    pub async fn from_bytes(bytes: impl Into<bytes::Bytes>) -> Result<AsyncDocument>;
    pub async fn with_backend(backend: impl Backend) -> Result<AsyncDocument>;
    pub fn version(&self) -> (u8, u8);
}
```

Crate-internal: `struct XrefIndex { entries: HashMap<u32, XrefEntry>, trailer: Dict, trailer_span: Span }`, `async fn load_xref_chain(fetcher: &Fetcher, start: u64) -> Result<(XrefIndex, Vec<SectionRecord>)>` (sections returned in chain order, newest→oldest).

**Steps:**

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `src/document.rs`:

```rust
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
    }

    #[tokio::test]
    async fn open_reads_from_disk() {
        let path = std::env::temp_dir().join(format!(
            "pdfboss-aio-doc-test-{}.pdf",
            std::process::id()
        ));
        std::fs::write(&path, simple_doc("from disk")).unwrap();
        let doc = AsyncDocument::open(&path).await.unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(doc.version(), (1, 7));
    }
```

Also add a compile-time thread-safety assertion just above the `tests` module (this is production code, not a test):

```rust
/// Compile-time guarantee that documents can be shared across tasks.
fn assert_document_is_shareable()
where
    AsyncDocument: Send + Sync + Clone,
{
}
```

(and call it nowhere — a `where`-bounded free function fails to compile the moment the bounds break; mark it `#[allow(dead_code)]`). Full item:

```rust
/// Compile-time guarantee that documents can be shared across tasks.
#[allow(dead_code)]
fn assert_document_is_shareable()
where
    AsyncDocument: Send + Sync + Clone,
{
}
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests::classic_document -- --nocapture
```

Expected failure: compile errors — `cannot find struct AsyncDocument`.

- [ ] **Step 3: Write minimal implementation.** Add to `src/document.rs` (extend the `use` block with `use std::collections::{HashMap, HashSet};`, `use std::path::Path;`, `use bytes::Bytes;`, `use crate::backend::{FileBackend, MemBackend};`, `use crate::cache::CachedBackend;`):

```rust
/// Merged cross-reference entries plus the merged trailer, mirroring the
/// sync loader's newest-wins semantics.
pub(crate) struct XrefIndex {
    pub(crate) entries: HashMap<u32, XrefEntry>,
    pub(crate) trailer: Dict,
    /// Span for the single merged `Trailer` element: the newest section's
    /// trailer region (classic), or that section's own span (stream) —
    /// adopted rule 4.
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
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) file_len: u64,
    pub(crate) version: (u8, u8),
    /// Span of the `%PDF-` header run; `None` when the first 1 KiB holds
    /// no header (the Header element is then omitted, adopted rule 1).
    pub(crate) header_span: Option<Span>,
    pub(crate) xref: XrefIndex,
    /// Sections in chain order — newest→oldest — for the element stream.
    pub(crate) sections: Vec<SectionRecord>,
    pub(crate) startxref: StartXrefRecord,
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
/// `/Prev` is followed. Visited offsets guard against loops. Sections come
/// back in chain order — newest→oldest, hybrid sections where the walk
/// visits them — for the element stream (adopted rule 4). The merged
/// trailer's span is the startxref section's trailer region.
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
        if let Some(hybrid_offset) = parsed.xrefstm.filter(|&v| v < fetcher.len) {
            if visited.insert(hybrid_offset) {
                // Lenient: a broken hybrid stream leaves the table alone.
                if let Ok(hybrid) = parse_section_at(fetcher, hybrid_offset).await {
                    merge_section(&mut entries, &mut trailer, &hybrid);
                    sections.push(hybrid.record);
                }
            }
        }
        next = parsed.prev.filter(|&v| v < fetcher.len);
        merge_section(&mut entries, &mut trailer, &parsed);
        sections.push(parsed.record);
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
```

Also add `pub use document::AsyncDocument;` to `src/lib.rs` re-exports (changed line):

```rust
pub use backend::{Backend, BoxFuture, FileBackend, MemBackend};
pub use cache::CachedBackend;
pub use document::AsyncDocument;
pub use error::{Error, Result};
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests -- --nocapture
```

Expected: all document tests pass, including `classic_document_matches_sync_xref`, `xref_stream_document_matches_sync_xref`, `prev_chain_merges_newest_wins`, `hybrid_xrefstm_beats_the_tables_free_entry`, `open_reads_from_disk`.

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): AsyncDocument open flow with span-only xref chain walk"
```

---

### Task 8: document.rs — get_object, resolve, object-stream cache

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/document.rs`
- Test: unit tests inside `src/document.rs`

**Interfaces:**
- Consumes: `AsyncDocument`/`DocumentInner`/`Fetcher` (Task 7), `Parser`, `NoResolve`, `Resolve`, `LengthProbe` (defined here), `pdfboss_core::filters::decode_stream`.
- Produces (public API pinned by the spec; `pub(crate)` items relied on by Tasks 9–12):

```rust
impl AsyncDocument {
    pub async fn get_object(&self, r: ObjRef) -> Result<Object>;
    pub async fn resolve(&self, o: &Object) -> Result<Object>;
    // crate-internal:
    pub(crate) fn fetch_object_cached<'a>(&'a self, r: ObjRef, chain: &'a mut Vec<u32>)
        -> BoxFuture<'a, Result<Object>>;
    pub(crate) async fn parse_in_file(&self, offset: u64, chain: &mut Vec<u32>)
        -> Result<(Span, Object)>;
    pub(crate) async fn objstm_cache(&self, stream_num: u32) -> Result<Arc<ObjStmCache>>;
    pub(crate) async fn decode_stream_with_chain(&self, s: &Stream, chain: &mut Vec<u32>)
        -> Result<Vec<u8>>;
    pub(crate) async fn resolve_with_chain(&self, o: &Object, chain: &mut Vec<u32>)
        -> Result<Object>;
}
pub(crate) struct ObjStmCache {
    pub(crate) container: ObjRef,
    pub(crate) container_span: Span,
    /* first, data, members */
}
impl ObjStmCache {
    pub(crate) fn object(&self, index: u32) -> Result<Object>;
    pub(crate) fn member_span(&self, index: u32) -> Result<Span>;
}
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `src/document.rs` (add `use pdfboss_testkit::multi_page_doc;` next to the existing testkit import):

```rust
    use pdfboss_core::{Object, ObjRef};
    use pdfboss_testkit::multi_page_doc;

    #[tokio::test]
    async fn objects_match_the_sync_document() {
        for data in [simple_doc("objects"), multi_page_doc(&["a", "b"])] {
            let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
            let doc = AsyncDocument::from_bytes(data).await.unwrap();
            for num in 1..=8u32 {
                let r = ObjRef { num, gen: 0 };
                match sync_doc.get(r) {
                    Ok(expected) => assert_eq!(
                        doc.get_object(r).await.unwrap(),
                        expected,
                        "object {num}"
                    ),
                    Err(_) => assert!(doc.get_object(r).await.is_err(), "object {num}"),
                }
            }
        }
    }

    #[tokio::test]
    async fn compressed_objects_are_fetched_from_their_container() {
        let (dict, payload) = pdfboss_testkit::objstm_payload(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ]);
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.stream(6, &dict, &payload);
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT (compressed) Tj ET");
        let data = b.build_xref_stream(1);
        let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        let font = doc.get_object(ObjRef { num: 5, gen: 0 }).await.unwrap();
        assert_eq!(font, sync_doc.get(ObjRef { num: 5, gen: 0 }).unwrap());
        assert_eq!(
            font.as_dict()
                .and_then(|d| d.get_name("BaseFont"))
                .map(|n| n.0.as_str()),
            Some("Helvetica")
        );
        let catalog = doc.get_object(ObjRef { num: 1, gen: 0 }).await.unwrap();
        assert_eq!(catalog, sync_doc.get(ObjRef { num: 1, gen: 0 }).unwrap());
    }

    #[tokio::test]
    async fn indirect_stream_length_triggers_one_extra_fetch() {
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.object(1, "<< /Type /Catalog >>");
        b.object(4, "<< /Length 7 0 R >>\nstream\nBT ET\nendstream");
        b.object(7, "5");
        let data = b.build(1);
        let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        let stream = doc.get_object(ObjRef { num: 4, gen: 0 }).await.unwrap();
        assert_eq!(stream, sync_doc.get(ObjRef { num: 4, gen: 0 }).unwrap());
        assert_eq!(stream.as_stream().unwrap().data, b"BT ET");
    }

    #[tokio::test]
    async fn objects_larger_than_the_initial_window_grow_until_complete() {
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.object(1, "<< /Type /Catalog >>");
        let big = vec![b'q'; 5000];
        b.stream(4, "", &big);
        let doc = AsyncDocument::from_bytes(b.build(1)).await.unwrap();
        let object = doc.get_object(ObjRef { num: 4, gen: 0 }).await.unwrap();
        assert_eq!(object.as_stream().unwrap().data, big);
    }

    #[tokio::test]
    async fn resolve_mirrors_sync_lenient_semantics() {
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.object(1, "<< /Type /Catalog >>");
        b.object(6, "6 0 R");
        let doc = AsyncDocument::from_bytes(b.build(1)).await.unwrap();
        let missing = Object::Ref(ObjRef { num: 99, gen: 0 });
        assert_eq!(doc.resolve(&missing).await.unwrap(), Object::Null);
        let loops = Object::Ref(ObjRef { num: 6, gen: 0 });
        assert!(matches!(
            doc.resolve(&loops).await,
            Err(Error::Core(pdfboss_core::Error::CircularReference(6)))
        ));
        // Generation mismatch is tolerated (lenient), like the sync model.
        let catalog = doc.get_object(ObjRef { num: 1, gen: 7 }).await.unwrap();
        assert!(catalog.as_dict().is_some());
    }
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests::objects_match -- --nocapture
```

Expected failure: compile error — `no method named get_object found for struct AsyncDocument`.

- [ ] **Step 3: Write minimal implementation.** First extend the `use` block of `src/document.rs` to its full new version:

```rust
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use pdfboss_core::elements::{Span, XrefKind};
use pdfboss_core::lexer::{Lexer, Token};
use pdfboss_core::parser::{NoResolve, Parser, Resolve};
use pdfboss_core::xref::XrefEntry;
use pdfboss_core::{Dict, ObjRef, Object, Stream};

use crate::backend::{Backend, BoxFuture, FileBackend, MemBackend};
use crate::cache::CachedBackend;
use crate::error::{Error, Result};
```

Replace `DocumentInner` and `from_arc` with their full new versions (two caches added):

```rust
pub(crate) struct DocumentInner {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) file_len: u64,
    pub(crate) version: (u8, u8),
    /// Span of the `%PDF-` header run; `None` when the first 1 KiB holds
    /// no header (the Header element is then omitted, adopted rule 1).
    pub(crate) header_span: Option<Span>,
    pub(crate) xref: XrefIndex,
    /// Sections in chain order — newest→oldest — for the element stream.
    pub(crate) sections: Vec<SectionRecord>,
    pub(crate) startxref: StartXrefRecord,
    pub(crate) eof_span: Option<Span>,
    /// Cache of fetched indirect objects.
    pub(crate) objects: std::sync::Mutex<HashMap<(u32, u16), Arc<Object>>>,
    /// Decoded object streams, keyed by container number, so a resident
    /// container is fetched, decoded and header-parsed once. The map lock
    /// is never held across a fetch (no deadlocks on nested containers);
    /// concurrent misses may decode twice, and the first insert wins.
    pub(crate) objstms: tokio::sync::Mutex<HashMap<u32, Arc<ObjStmCache>>>,
}
```

```rust
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
            objects: std::sync::Mutex::new(HashMap::new()),
            objstms: tokio::sync::Mutex::new(HashMap::new()),
        };
        Ok(AsyncDocument {
            inner: Arc::new(inner),
        })
    }
```

Then add the new code below the `AsyncDocument` impl from Task 7:

```rust
/// Initial object window, doubling until the object parses completely.
const OBJECT_WINDOW: usize = 2048;
/// Reference-chase depth limit, mirroring the sync document model.
const MAX_RESOLVE_DEPTH: usize = 32;

/// Resolver for window parsing: answers the one known `/Length` value and
/// records the first reference it could not answer, so the caller can
/// fetch it and re-parse.
struct LengthProbe {
    known: Option<(ObjRef, i64)>,
    missing: std::cell::Cell<Option<ObjRef>>,
}

impl LengthProbe {
    fn new(known: Option<(ObjRef, i64)>) -> LengthProbe {
        LengthProbe {
            known,
            missing: std::cell::Cell::new(None),
        }
    }

    fn missing(&self) -> Option<ObjRef> {
        self.missing.get()
    }
}

impl Resolve for LengthProbe {
    fn resolve_ref(&self, r: ObjRef) -> Option<Object> {
        match self.known {
            Some((known_ref, value)) if known_ref == r => Some(Object::Int(value)),
            _ => {
                if self.missing.get().is_none() {
                    self.missing.set(Some(r));
                }
                None
            }
        }
    }
}

/// True when `object` is a stream whose declared `/Length` (direct, or the
/// known indirect value) does not match the bytes the parser captured —
/// the signature of a stream cut by the window edge, which fell into the
/// lenient recovery path the sync parser would not have taken.
fn stream_dishonors_length(object: &Object, known_length: Option<(ObjRef, i64)>) -> bool {
    let Some(stream) = object.as_stream() else {
        return false;
    };
    let declared = match stream.dict.get("Length") {
        Some(Object::Int(n)) => Some(*n),
        Some(Object::Ref(r)) => {
            known_length.and_then(|(known_ref, value)| (known_ref == *r).then_some(value))
        }
        _ => None,
    };
    match declared {
        Some(length) if length >= 0 => stream.data.len() as u64 != length as u64,
        _ => false,
    }
}

/// Human-readable object type name for error messages.
fn object_type_name(o: &Object) -> &'static str {
    match o {
        Object::Null => "null",
        Object::Bool(_) => "boolean",
        Object::Int(_) => "integer",
        Object::Real(_) => "real",
        Object::String(_) => "string",
        Object::Name(_) => "name",
        Object::Array(_) => "array",
        Object::Dict(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Ref(_) => "reference",
    }
}

/// A fetched and decoded object stream: the container's physical span, the
/// decoded bytes, and each member's number and offset (ISO 32000 §7.5.7).
pub(crate) struct ObjStmCache {
    pub(crate) container: ObjRef,
    pub(crate) container_span: Span,
    first: usize,
    data: Vec<u8>,
    /// (object number, offset relative to `first`) per member, in header
    /// order.
    pub(crate) members: Vec<(u32, usize)>,
}

impl ObjStmCache {
    /// Parses member `index` out of the decoded bytes.
    pub(crate) fn object(&self, index: u32) -> Result<Object> {
        let start = self.member_start(index)?;
        Parser::at(&self.data, start)
            .parse_object(&NoResolve)
            .map_err(Error::Core)
    }

    /// Member `index`'s byte range within the decoded stream: from its
    /// header offset to the parser position after its last token.
    pub(crate) fn member_span(&self, index: u32) -> Result<Span> {
        let start = self.member_start(index)?;
        let mut parser = Parser::at(&self.data, start);
        parser.parse_object(&NoResolve).map_err(Error::Core)?;
        Ok(Span {
            start: start as u64,
            end: parser.pos() as u64,
        })
    }

    /// Absolute start of member `index` within the decoded bytes.
    fn member_start(&self, index: u32) -> Result<usize> {
        let offset = self
            .members
            .get(index as usize)
            .map(|entry| entry.1)
            .ok_or_else(|| {
                Error::Core(pdfboss_core::Error::Other(format!(
                    "object stream index {index} out of range (N = {})",
                    self.members.len()
                )))
            })?;
        self.first
            .checked_add(offset)
            .filter(|&pos| pos <= self.data.len())
            .ok_or_else(|| {
                Error::Core(pdfboss_core::Error::Other(format!(
                    "object stream offset {offset} lies outside the stream"
                )))
            })
    }
}

/// Parses the object-stream header: `2*n` integers, pairs of object number
/// and byte offset relative to `/First` (ISO 32000 §7.5.7).
fn parse_objstm_header(data: &[u8], n: usize) -> Result<Vec<(u32, usize)>> {
    let mut lexer = Lexer::new(data);
    let mut members = Vec::with_capacity(n);
    for _ in 0..n {
        let num = expect_header_int(&mut lexer)?;
        let offset = expect_header_int(&mut lexer)?;
        members.push((u32::try_from(num).unwrap_or(u32::MAX), offset));
    }
    Ok(members)
}

/// Reads one non-negative integer from the object-stream header.
fn expect_header_int(lexer: &mut Lexer) -> Result<usize> {
    match lexer.next_token().map_err(Error::Core)? {
        Token::Int(v) if v >= 0 => Ok(v as usize),
        _ => Err(Error::Core(pdfboss_core::Error::Syntax {
            offset: lexer.pos(),
            msg: "malformed object stream header".to_string(),
        })),
    }
}

/// A [`Resolve`] over a prefetched reference map.
struct MapResolve(HashMap<ObjRef, Object>);

impl Resolve for MapResolve {
    fn resolve_ref(&self, r: ObjRef) -> Option<Object> {
        self.0.get(&r).cloned()
    }
}
```

Then add a second `impl AsyncDocument` block with the fetching machinery:

```rust
impl AsyncDocument {
    /// Fetches an indirect object by reference (xref lookup, object-stream
    /// indirection, cached). A generation mismatch between the request and
    /// the file is tolerated (lenient), mirroring the sync document.
    pub async fn get_object(&self, r: ObjRef) -> Result<Object> {
        let mut chain = Vec::new();
        self.fetch_object_cached(r, &mut chain).await
    }

    /// Chases reference chains with a depth guard (beyond that:
    /// `CircularReference`); a reference to a missing or unreadable object
    /// resolves to `Null` (lenient), mirroring the sync document.
    pub async fn resolve(&self, o: &Object) -> Result<Object> {
        let mut chain = Vec::new();
        self.resolve_with_chain(o, &mut chain).await
    }

    /// Cached fetch. `chain` carries the object numbers currently being
    /// loaded up this call path, guarding re-entrant fetches (e.g. a
    /// stream whose `/Length` refers back to the stream itself) without
    /// blocking unrelated concurrent fetches of the same object.
    pub(crate) fn fetch_object_cached<'a>(
        &'a self,
        r: ObjRef,
        chain: &'a mut Vec<u32>,
    ) -> BoxFuture<'a, Result<Object>> {
        Box::pin(async move {
            if let Some(cached) = self
                .inner
                .objects
                .lock()
                .expect("object cache mutex")
                .get(&(r.num, r.gen))
            {
                return Ok((**cached).clone());
            }
            if chain.contains(&r.num) {
                return Err(Error::Core(pdfboss_core::Error::CircularReference(r.num)));
            }
            chain.push(r.num);
            let outcome = self.load_object(r, chain).await;
            chain.pop();
            let object = outcome?;
            self.inner
                .objects
                .lock()
                .expect("object cache mutex")
                .insert((r.num, r.gen), Arc::new(object.clone()));
            Ok(object)
        })
    }

    /// Uncached fetch: parses the object at its file offset or extracts it
    /// from its containing object stream.
    async fn load_object(&self, r: ObjRef, chain: &mut Vec<u32>) -> Result<Object> {
        match self.inner.xref.entries.get(&r.num).copied() {
            None | Some(XrefEntry::Free) => Err(Error::Core(
                pdfboss_core::Error::ObjectNotFound(r.num, r.gen),
            )),
            Some(XrefEntry::InFile { offset, .. }) => {
                let parsed = self.parse_in_file(offset, chain).await?;
                Ok(parsed.1)
            }
            Some(XrefEntry::InStream { stream_num, index }) => {
                let cache = self.objstm_cache_with_chain(stream_num, chain).await?;
                cache.object(index)
            }
        }
    }

    /// Parses the indirect object at `offset` from a growing window (2 KiB
    /// doubling), returning the object and its physical span
    /// (`N G obj … endobj`, end-exclusive). An indirect `/Length` triggers
    /// exactly one extra object fetch, then a re-parse with the value
    /// known. The parse is only accepted when it provably matches what the
    /// sync parser would produce on the whole file: slack after the parse
    /// end (or true end of file), and stream data honoring its declared
    /// length.
    pub(crate) async fn parse_in_file(
        &self,
        offset: u64,
        chain: &mut Vec<u32>,
    ) -> Result<(Span, Object)> {
        if offset >= self.inner.file_len {
            return Err(Error::Core(pdfboss_core::Error::Other(format!(
                "object offset {offset} lies outside the file"
            ))));
        }
        let fetcher = self.fetcher();
        let mut window = OBJECT_WINDOW;
        let mut known_length: Option<(ObjRef, i64)> = None;
        loop {
            let buf = fetcher.window(offset, window).await?;
            let at_eof = offset + buf.len() as u64 >= self.inner.file_len;
            let probe = LengthProbe::new(known_length);
            let mut parser = Parser::at(&buf, 0);
            match parser.parse_indirect(&probe) {
                Ok((_, object)) => {
                    let end = parser.pos();
                    if end + PARSE_SLACK <= buf.len() || at_eof {
                        if let Some(missing) = probe.missing() {
                            if let Ok(length_object) =
                                self.fetch_object_cached(missing, chain).await
                            {
                                if let Some(value) = length_object.as_int() {
                                    known_length = Some((missing, value));
                                    continue;
                                }
                            }
                            // Unresolvable length: the recovery-scan result
                            // stands, exactly as in the sync parser.
                        }
                        if at_eof || !stream_dishonors_length(&object, known_length) {
                            return Ok((
                                Span {
                                    start: offset,
                                    end: offset + end as u64,
                                },
                                object,
                            ));
                        }
                    }
                }
                Err(parse_error) => {
                    if at_eof {
                        return Err(Error::Core(parse_error));
                    }
                }
            }
            window = window.saturating_mul(2);
        }
    }

    /// The decoded container for object stream `stream_num`, fetched,
    /// decoded and header-parsed at most once.
    pub(crate) async fn objstm_cache(&self, stream_num: u32) -> Result<Arc<ObjStmCache>> {
        let mut chain = Vec::new();
        self.objstm_cache_with_chain(stream_num, &mut chain).await
    }

    async fn objstm_cache_with_chain(
        &self,
        stream_num: u32,
        chain: &mut Vec<u32>,
    ) -> Result<Arc<ObjStmCache>> {
        // Circularity is checked first: a reference chain leading back into
        // a container being decoded must fail fast. The map lock is never
        // held across the build below, so nested container fetches can
        // never deadlock on it; concurrent misses may decode a container
        // twice, and the first insert wins (correctness is unaffected).
        if chain.contains(&stream_num) {
            return Err(Error::Core(pdfboss_core::Error::CircularReference(
                stream_num,
            )));
        }
        if let Some(hit) = self.inner.objstms.lock().await.get(&stream_num) {
            return Ok(Arc::clone(hit));
        }
        let offset = match self.inner.xref.entries.get(&stream_num).copied() {
            Some(XrefEntry::InFile { offset, .. }) => offset,
            // A container cannot itself live in an object stream
            // (ISO 32000 §7.5.7), and a free or absent one has no bytes.
            Some(XrefEntry::InStream { .. }) | Some(XrefEntry::Free) | None => {
                return Err(Error::Core(pdfboss_core::Error::ObjectNotFound(
                    stream_num, 0,
                )))
            }
        };
        chain.push(stream_num);
        let outcome = self.build_objstm_cache(stream_num, offset, chain).await;
        chain.pop();
        let entry = outcome?;
        let mut cache = self.inner.objstms.lock().await;
        let stored = cache
            .entry(stream_num)
            .or_insert_with(|| Arc::clone(&entry));
        Ok(Arc::clone(stored))
    }

    /// Fetches, decodes and header-parses one container. `chain` already
    /// carries the container's number.
    async fn build_objstm_cache(
        &self,
        stream_num: u32,
        offset: u64,
        chain: &mut Vec<u32>,
    ) -> Result<Arc<ObjStmCache>> {
        let (container_span, object) = self.parse_in_file(offset, chain).await?;
        let stream = object
            .as_stream()
            .ok_or(Error::Core(pdfboss_core::Error::TypeMismatch {
                expected: "stream",
                found: object_type_name(&object),
            }))?;
        let n = self
            .resolve_with_chain(stream.dict.get("N").unwrap_or(&Object::Null), chain)
            .await?
            .as_int()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(Error::Core(pdfboss_core::Error::MissingKey("N")))?;
        let first = self
            .resolve_with_chain(stream.dict.get("First").unwrap_or(&Object::Null), chain)
            .await?
            .as_int()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(Error::Core(pdfboss_core::Error::MissingKey("First")))?;
        let data = self.decode_stream_with_chain(stream, chain).await?;
        let members = parse_objstm_header(&data, n)?;
        Ok(Arc::new(ObjStmCache {
            container: ObjRef {
                num: stream_num,
                gen: 0,
            },
            container_span,
            first,
            data,
            members,
        }))
    }

    /// Chain-threaded resolve (see [`AsyncDocument::resolve`]).
    pub(crate) async fn resolve_with_chain(
        &self,
        o: &Object,
        chain: &mut Vec<u32>,
    ) -> Result<Object> {
        let mut current = o.clone();
        let mut last_num = 0;
        for _ in 0..MAX_RESOLVE_DEPTH {
            match current {
                Object::Ref(r) => {
                    last_num = r.num;
                    current = match self.fetch_object_cached(r, chain).await {
                        Ok(object) => object,
                        Err(Error::Core(pdfboss_core::Error::CircularReference(n))) => {
                            return Err(Error::Core(pdfboss_core::Error::CircularReference(n)))
                        }
                        Err(_) => return Ok(Object::Null),
                    };
                }
                other => return Ok(other),
            }
        }
        Err(Error::Core(pdfboss_core::Error::CircularReference(
            last_num,
        )))
    }

    /// Decodes a stream through its filter chain. The sync filter pipeline
    /// resolves references synchronously, so every reference reachable
    /// from the filter-relevant dict keys is fetched up front into a map
    /// the pipeline can consult.
    pub(crate) async fn decode_stream_with_chain(
        &self,
        s: &Stream,
        chain: &mut Vec<u32>,
    ) -> Result<Vec<u8>> {
        let resolver = self.prefetch_filter_refs(&s.dict, chain).await;
        pdfboss_core::filters::decode_stream(s, &resolver).map_err(Error::Core)
    }

    /// Transitively resolves references reachable from the stream dict's
    /// filter-relevant keys (bounded rounds; failures resolve to Null,
    /// lenient).
    async fn prefetch_filter_refs(&self, dict: &Dict, chain: &mut Vec<u32>) -> MapResolve {
        const FILTER_KEYS: [&str; 5] = ["Length", "Filter", "DecodeParms", "DP", "F"];
        let mut map: HashMap<ObjRef, Object> = HashMap::new();
        let mut frontier: Vec<Object> = FILTER_KEYS
            .iter()
            .filter_map(|key| dict.get(key).cloned())
            .collect();
        for _ in 0..MAX_RESOLVE_DEPTH {
            let mut next = Vec::new();
            for value in frontier.drain(..) {
                match value {
                    Object::Ref(r) => {
                        if !map.contains_key(&r) {
                            let resolved = match self.fetch_object_cached(r, chain).await {
                                Ok(object) => object,
                                Err(_) => Object::Null,
                            };
                            next.push(resolved.clone());
                            map.insert(r, resolved);
                        }
                    }
                    Object::Array(items) => next.extend(items),
                    Object::Dict(d) => next.extend(d.iter().map(|(_, v)| v.clone())),
                    _ => {}
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        MapResolve(map)
    }
}
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests -- --nocapture
```

Expected: all document tests pass, including the five new ones (`objects_match_the_sync_document`, `compressed_objects_are_fetched_from_their_container`, `indirect_stream_length_triggers_one_extra_fetch`, `objects_larger_than_the_initial_window_grow_until_complete`, `resolve_mirrors_sync_lenient_semantics`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): windowed get_object, resolve and object-stream cache"
```

---

### Task 9: document.rs — decode_stream, read_span, metadata

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/document.rs`
- Test: unit tests inside `src/document.rs`

**Interfaces:**
- Consumes: `decode_stream_with_chain`, `resolve` (Task 8), `Fetcher::read_range` (Task 5), `pdfboss_core::Metadata`, `pdfboss_core::object::decode_text_string`.
- Produces (public API pinned by the spec; relied on by Tasks 11–13 and plans 03/04/05):

```rust
impl AsyncDocument {
    pub async fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>>;
    pub async fn read_span(&self, span: Span) -> Result<Vec<u8>>;
    pub fn file_len(&self) -> u64;
    pub async fn metadata(&self) -> Result<Metadata>;
}
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `src/document.rs`:

```rust
    #[tokio::test]
    async fn decode_stream_matches_sync_stream_data() {
        let data = simple_doc("stream parity");
        let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        let object = doc.get_object(ObjRef { num: 4, gen: 0 }).await.unwrap();
        let stream = object.as_stream().unwrap();
        assert_eq!(
            doc.decode_stream(stream).await.unwrap(),
            sync_doc.stream_data(stream).unwrap()
        );
    }

    #[tokio::test]
    async fn read_span_returns_raw_file_bytes() {
        let data = simple_doc("raw bytes");
        let doc = AsyncDocument::from_bytes(data.clone()).await.unwrap();
        let slice = doc
            .read_span(Span { start: 0, end: 8 })
            .await
            .unwrap();
        assert_eq!(slice, b"%PDF-1.7");
        // Spans are clamped to the file length, which is also public.
        let file_len = data.len() as u64;
        assert_eq!(doc.file_len(), file_len);
        let tail = doc
            .read_span(Span {
                start: file_len - 6,
                end: file_len + 50,
            })
            .await
            .unwrap();
        assert_eq!(tail, b"%%EOF\n");
        assert!(doc
            .read_span(Span {
                start: file_len + 1,
                end: file_len + 2
            })
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn metadata_matches_the_sync_document() {
        let mut b = pdfboss_testkit::PdfBuilder::new().trailer_extra("/Info 6 0 R");
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        // /Title is UTF-16BE with BOM; /Author is a plain string.
        b.object(6, "<< /Title <FEFF00480151> /Author (plain author) >>");
        let data = b.build(1);
        let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        let meta = doc.metadata().await.unwrap();
        assert_eq!(meta, sync_doc.metadata());
        assert_eq!(meta.title.as_deref(), Some("H\u{151}"));
        assert_eq!(meta.author.as_deref(), Some("plain author"));
        assert_eq!(meta.subject, None);
    }

    #[tokio::test]
    async fn metadata_without_info_is_all_none() {
        let doc = AsyncDocument::from_bytes(simple_doc("x")).await.unwrap();
        assert_eq!(doc.metadata().await.unwrap(), pdfboss_core::Metadata::default());
    }
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests::decode_stream_matches -- --nocapture
```

Expected failure: compile error — `no method named decode_stream found for struct AsyncDocument`.

- [ ] **Step 3: Write minimal implementation.** Add `use pdfboss_core::object::decode_text_string;` and `use pdfboss_core::Metadata;` to the `use` block, then add to the second `impl AsyncDocument` block (from Task 8):

```rust
    /// Decodes a stream's data through its filter chain, resolving indirect
    /// filter parameters against this document.
    pub async fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>> {
        let mut chain = Vec::new();
        self.decode_stream_with_chain(s, &mut chain).await
    }

    /// Raw file bytes for `span` (for hex views), clamped to the file
    /// length.
    pub async fn read_span(&self, span: Span) -> Result<Vec<u8>> {
        let start = span.start.min(self.inner.file_len);
        let end = span.end.min(self.inner.file_len);
        if start >= end {
            return Ok(Vec::new());
        }
        self.fetcher().read_range(start, end).await
    }

    /// Total length of the underlying file in bytes.
    pub fn file_len(&self) -> u64 {
        self.inner.file_len
    }

    /// Document metadata from the trailer `/Info` dictionary (lenient:
    /// absent or malformed entries are simply `None`), mirroring the sync
    /// document.
    pub async fn metadata(&self) -> Result<Metadata> {
        let mut meta = Metadata::default();
        let Some(info) = self.inner.xref.trailer.get("Info") else {
            return Ok(meta);
        };
        let Ok(info) = self.resolve(info).await else {
            return Ok(meta);
        };
        let Some(dict) = info.as_dict() else {
            return Ok(meta);
        };
        meta.title = self.meta_string(dict, "Title").await;
        meta.author = self.meta_string(dict, "Author").await;
        meta.subject = self.meta_string(dict, "Subject").await;
        meta.keywords = self.meta_string(dict, "Keywords").await;
        meta.creator = self.meta_string(dict, "Creator").await;
        meta.producer = self.meta_string(dict, "Producer").await;
        meta.creation_date = self.meta_string(dict, "CreationDate").await;
        meta.mod_date = self.meta_string(dict, "ModDate").await;
        Ok(meta)
    }

    /// Reads `key` from an info dictionary as a decoded text string.
    async fn meta_string(&self, dict: &Dict, key: &str) -> Option<String> {
        let value = self.resolve(dict.get(key)?).await.ok()?;
        Some(decode_text_string(value.as_str_bytes()?))
    }
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests -- --nocapture
```

Expected: all document tests pass, including the four new ones (`decode_stream_matches_sync_stream_data`, `read_span_returns_raw_file_bytes`, `metadata_matches_the_sync_document`, `metadata_without_info_is_all_none`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): decode_stream, read_span and metadata on AsyncDocument"
```

---

### Task 10: document.rs — page-tree index and page_count

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/document.rs`
- Test: unit tests inside `src/document.rs`

**Interfaces:**
- Consumes: `resolve_with_chain` (Task 8), `DocumentInner`/`from_arc` (Tasks 7–8).
- Produces (public `page_count` pinned by the spec; `PageRecord` relied on by Tasks 11–12):

```rust
impl AsyncDocument {
    pub fn page_count(&self) -> usize;
    pub(crate) fn page_record(&self, index: usize) -> Option<PageRecord>;
}
pub(crate) struct PageRecord {
    /// The leaf's indirect reference; `None` for a page dict inlined
    /// directly into `/Kids` (no `ObjRef` exists for it).
    pub(crate) r: Option<ObjRef>,
    pub(crate) dict: Dict,
    /// Inherited `/Resources` (ISO 32000 §7.7.3.4), possibly empty.
    pub(crate) resources: Dict,
}
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `src/document.rs`:

```rust
    #[tokio::test]
    async fn page_count_matches_the_sync_document() {
        for (data, expected) in [
            (simple_doc("one"), 1usize),
            (multi_page_doc(&["a", "b", "c"]), 3usize),
        ] {
            let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
            let doc = AsyncDocument::from_bytes(data).await.unwrap();
            assert_eq!(doc.page_count(), expected);
            assert_eq!(doc.page_count(), sync_doc.page_count());
        }
    }

    #[tokio::test]
    async fn page_records_carry_inherited_resources_and_refs() {
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.object(3, "<< /Type /Page /Parent 2 0 R >>");
        b.object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let doc = AsyncDocument::from_bytes(b.build(1)).await.unwrap();
        assert_eq!(doc.page_count(), 1);
        let record = doc.page_record(0).unwrap();
        assert_eq!(record.r, Some(ObjRef { num: 3, gen: 0 }));
        assert!(record.resources.get("Font").is_some(), "inherited resources");
        assert!(doc.page_record(1).is_none());
    }

    #[tokio::test]
    async fn kids_cycle_truncates_without_hanging() {
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        // 2 → 3 → {4, back to 2}: the back-edge must be ignored.
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Pages /Kids [4 0 R 2 0 R] /Count 1 >>");
        b.object(4, "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 100 100] >>");
        let doc = AsyncDocument::from_bytes(b.build(1)).await.unwrap();
        assert_eq!(doc.page_count(), 1, "cycle back-edge yields no extra pages");
    }

    #[tokio::test]
    async fn page_count_is_the_flattened_length() {
        // The tree declares five pages but supplies one kid. The async
        // document always flattens at open, so — per adopted rule 6 — it
        // reports the authoritative flattened length (the sync document
        // reports the declared /Count until its tree is flattened).
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 5 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let doc = AsyncDocument::from_bytes(b.build(1)).await.unwrap();
        assert_eq!(doc.page_count(), 1);
    }
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests::page_count_matches -- --nocapture
```

Expected failure: compile error — `no method named page_count found for struct AsyncDocument`.

- [ ] **Step 3: Write minimal implementation.** Replace `DocumentInner` with its full new version (one field added at the end):

```rust
pub(crate) struct DocumentInner {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) file_len: u64,
    pub(crate) version: (u8, u8),
    /// Span of the `%PDF-` header run; `None` when the first 1 KiB holds
    /// no header (the Header element is then omitted, adopted rule 1).
    pub(crate) header_span: Option<Span>,
    pub(crate) xref: XrefIndex,
    /// Sections in chain order — newest→oldest — for the element stream.
    pub(crate) sections: Vec<SectionRecord>,
    pub(crate) startxref: StartXrefRecord,
    pub(crate) eof_span: Option<Span>,
    /// Cache of fetched indirect objects.
    pub(crate) objects: std::sync::Mutex<HashMap<(u32, u16), Arc<Object>>>,
    /// Decoded object streams, keyed by container number, so a resident
    /// container is fetched, decoded and header-parsed once. The map lock
    /// is never held across a fetch (no deadlocks on nested containers);
    /// concurrent misses may decode twice, and the first insert wins.
    pub(crate) objstms: tokio::sync::Mutex<HashMap<u32, Arc<ObjStmCache>>>,
    /// The flattened page tree, set exactly once at the end of the open
    /// flow (fetching only catalog and tree nodes).
    pub(crate) pages: std::sync::OnceLock<Vec<PageRecord>>,
}
```

Replace the tail of `from_arc` (after `let inner = DocumentInner { … };`) with its full new version:

```rust
        let inner = DocumentInner {
            backend,
            file_len,
            version,
            header_span,
            xref,
            sections,
            startxref,
            eof_span,
            objects: std::sync::Mutex::new(HashMap::new()),
            objstms: tokio::sync::Mutex::new(HashMap::new()),
            pages: std::sync::OnceLock::new(),
        };
        let doc = AsyncDocument {
            inner: Arc::new(inner),
        };
        let pages = doc.flatten_pages().await;
        doc.inner
            .pages
            .set(pages)
            .expect("page index is set exactly once at open");
        Ok(doc)
    }
```

Add the record type near `SectionRecord`:

```rust
/// The flattened record for one page leaf: its reference (when the leaf
/// was reached through one), its dictionary, and its inherited
/// `/Resources` (ISO 32000 §7.7.3.4).
#[derive(Clone, Debug)]
pub(crate) struct PageRecord {
    pub(crate) r: Option<ObjRef>,
    pub(crate) dict: Dict,
    pub(crate) resources: Dict,
}

/// Page-tree traversal depth cap, mirroring the sync document model.
const MAX_TREE_DEPTH: usize = 256;
```

Add to the second `impl AsyncDocument` block:

```rust
    /// Number of pages: the flattened page tree's length. The tree is
    /// flattened once at open, so this is synchronous and authoritative —
    /// mirroring the sync document once its tree has been flattened.
    pub fn page_count(&self) -> usize {
        self.inner.pages.get().map_or(0, Vec::len)
    }

    /// The flattened record for the page at 0-based `index`.
    pub(crate) fn page_record(&self, index: usize) -> Option<PageRecord> {
        self.inner
            .pages
            .get()
            .and_then(|pages| pages.get(index))
            .cloned()
    }

    /// Flattens the page tree by iterative depth-first traversal of
    /// `/Kids` with a visited-reference cycle guard and a depth cap,
    /// carrying inherited `/Resources`. Any structural problem simply
    /// truncates or skips (lenient) — this never fails. Only the catalog
    /// and tree nodes are fetched; page content is not.
    async fn flatten_pages(&self) -> Vec<PageRecord> {
        let mut chain = Vec::new();
        let mut pages = Vec::new();
        let Some(root) = self.inner.xref.trailer.get("Root") else {
            return pages;
        };
        let Ok(catalog) = self.resolve_with_chain(root, &mut chain).await else {
            return pages;
        };
        let Some(tree_root) = catalog.as_dict().and_then(|d| d.get("Pages")).cloned() else {
            return pages;
        };
        let mut visited: HashSet<ObjRef> = HashSet::new();
        let mut stack: Vec<(Object, Dict, usize)> = vec![(tree_root, Dict::new(), 0)];
        while let Some((node, inherited_resources, depth)) = stack.pop() {
            if depth > MAX_TREE_DEPTH {
                continue;
            }
            let node_ref = node.as_ref();
            if let Some(r) = node_ref {
                if !visited.insert(r) {
                    continue; // cycle: this node was already traversed
                }
            }
            let Ok(resolved) = self.resolve_with_chain(&node, &mut chain).await else {
                continue;
            };
            let Some(dict) = resolved.as_dict() else {
                continue;
            };
            let resources = match dict.get("Resources") {
                Some(value) => match self.resolve_with_chain(value, &mut chain).await {
                    Ok(resolved_resources) => resolved_resources
                        .as_dict()
                        .cloned()
                        .unwrap_or(inherited_resources),
                    Err(_) => inherited_resources,
                },
                None => inherited_resources,
            };
            let is_page = dict.get_name("Type").is_some_and(|n| n.0 == "Page");
            let kids = if is_page {
                None
            } else {
                self.array_value(dict, "Kids", &mut chain).await
            };
            match kids {
                Some(kids) => {
                    // Reverse push so pop order matches document order.
                    for kid in kids.iter().rev() {
                        stack.push((kid.clone(), resources.clone(), depth + 1));
                    }
                }
                None => pages.push(PageRecord {
                    r: node_ref,
                    dict: dict.clone(),
                    resources,
                }),
            }
        }
        pages
    }

    /// Resolves `dict[key]` to an array, if present and well-formed.
    async fn array_value(
        &self,
        dict: &Dict,
        key: &str,
        chain: &mut Vec<u32>,
    ) -> Option<Vec<Object>> {
        match self.resolve_with_chain(dict.get(key)?, chain).await.ok()? {
            Object::Array(items) => Some(items),
            _ => None,
        }
    }
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio document::tests -- --nocapture
```

Expected: all document tests pass, including the four new ones (`page_count_matches_the_sync_document`, `page_records_carry_inherited_resources_and_refs`, `kids_cycle_truncates_without_hanging`, `page_count_is_the_flattened_length`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): page-tree index built at open with span-only fetches"
```

---

### Task 11: stream.rs — ElementStream, physical layer

**Files:**
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/stream.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/document.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`
- Test: unit tests inside `src/stream.rs`

**Interfaces:**
- Consumes: `AsyncDocument` internals from Tasks 7–10 (`sections`, `startxref`, `eof_span`, `header_span`, `xref.entries`, `parse_in_file`, `objstm_cache`, `ObjStmCache::{object, member_span, container, container_span}`), `pdfboss_core::elements::{Element, ElementOpts, Span, XrefKind}`.
- Produces (public API pinned by the spec; extended by Task 12, consumed by Task 13 and plans 03/04/05):

```rust
pub struct ElementStream<'a> { /* BoxStream over the element state machine */ }
impl<'a> futures_core::Stream for ElementStream<'a> { type Item = Result<Element>; }
impl AsyncDocument { pub fn elements(&self, opts: ElementOpts) -> ElementStream<'_>; }
// crate-internal accessors added to document.rs:
impl AsyncDocument {
    pub(crate) fn header_span(&self) -> Option<Span>;
    pub(crate) fn xref_entries(&self) -> Vec<(u32, XrefEntry)>;
    pub(crate) fn sections(&self) -> &[SectionRecord];
    pub(crate) fn merged_trailer(&self) -> (Dict, Span);
    pub(crate) fn startxref_record(&self) -> (u64, Span);
    pub(crate) fn eof_span(&self) -> Option<Span>;
    pub(crate) async fn physical_object(&self, r: ObjRef, offset: u64) -> Result<(Span, Object)>;
}
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Create `src/stream.rs`:

```rust
//! A lazy element stream mirroring the sync iterator's ordering and
//! salvage semantics: physical elements first (header when present,
//! objects by offset with object-stream members after their container,
//! xref sections in chain order, one merged trailer, startxref, eof),
//! then logical elements in document order. Nothing is fetched, parsed or
//! decoded before it is yielded; logical elements are prepared one page
//! at a time.

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use pdfboss_core::elements::{Element, ElementOpts, XrefKind};
    use pdfboss_core::ObjRef;
    use pdfboss_testkit::simple_doc;

    use crate::document::AsyncDocument;
    use crate::error::Result;

    fn physical_opts() -> ElementOpts {
        ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        }
    }

    async fn collect(doc: &AsyncDocument, opts: ElementOpts) -> Vec<Result<Element>> {
        let mut stream = doc.elements(opts);
        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            items.push(item);
        }
        items
    }

    #[tokio::test]
    async fn physical_sequence_shape_for_a_classic_document() {
        let data = simple_doc("elements");
        let file_len = data.len() as u64;
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        let elements: Vec<Element> = collect(&doc, physical_opts())
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        // `%PDF-1.7` at offset 0: the header span covers the version run.
        assert!(matches!(
            elements[0],
            Element::Header { version: (1, 7), span } if span.start == 0 && span.end == 8
        ));
        let object_numbers: Vec<u32> = elements
            .iter()
            .filter_map(|el| match el {
                Element::IndirectObject { r, .. } => Some(r.num),
                _ => None,
            })
            .collect();
        assert_eq!(object_numbers, vec![1, 2, 3, 4, 5], "objects in offset order");
        let mut previous_end = 0;
        for element in &elements {
            if let Element::IndirectObject { span, .. } = element {
                assert!(span.start >= previous_end, "object spans are disjoint");
                assert!(span.end <= file_len, "spans stay in bounds");
                previous_end = span.end;
            }
        }
        // Tail shape (adopted rule 4): xref, trailer, startxref, eof.
        assert!(matches!(
            elements[elements.len() - 4],
            Element::XrefSection { kind: XrefKind::Table, entries: 6, .. }
        ));
        assert!(matches!(
            &elements[elements.len() - 3],
            Element::Trailer { dict, .. } if dict.get("Root").is_some()
        ));
        assert!(matches!(
            elements[elements.len() - 2],
            Element::StartXref { .. }
        ));
        assert!(
            matches!(elements[elements.len() - 1], Element::Eof { span } if span.end == file_len)
        );
    }

    #[tokio::test]
    async fn objstm_members_follow_their_container() {
        let (dict, payload) = pdfboss_testkit::objstm_payload(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (5, "(member)"),
        ]);
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.stream(6, &dict, &payload);
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let doc = AsyncDocument::from_bytes(b.build_xref_stream(1))
            .await
            .unwrap();
        let elements: Vec<Element> = collect(&doc, physical_opts())
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let object_sequence: Vec<(u32, bool)> = elements
            .iter()
            .filter_map(|el| match el {
                Element::IndirectObject { r, in_objstm, .. } => {
                    Some((r.num, in_objstm.is_some()))
                }
                _ => None,
            })
            .collect();
        let container_pos = object_sequence
            .iter()
            .position(|&(num, is_member)| num == 6 && !is_member)
            .expect("container element present");
        assert_eq!(object_sequence[container_pos + 1], (1, true));
        assert_eq!(object_sequence[container_pos + 2], (5, true));
        let member = elements
            .iter()
            .find_map(|el| match el {
                Element::IndirectObject {
                    r,
                    in_objstm: Some((container, member_span)),
                    ..
                } if r.num == 5 => Some((*container, *member_span)),
                _ => None,
            })
            .expect("member element present");
        assert_eq!(member.0, ObjRef { num: 6, gen: 0 });
        assert!(member.1.start < member.1.end);
    }

    #[tokio::test]
    async fn broken_objects_yield_err_and_the_stream_continues() {
        // Corrupt one object header without moving offsets: object 5's
        // header keyword becomes garbage of equal length.
        let mut data = simple_doc("salvage");
        let pos = data
            .windows(b"5 0 obj".len())
            .position(|w| w == b"5 0 obj")
            .unwrap();
        data[pos..pos + 7].copy_from_slice(b"5 0 ob!");
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        let items = collect(&doc, physical_opts()).await;
        assert!(
            items.iter().any(|item| item.is_err()),
            "the bad object surfaces as Err"
        );
        let good: Vec<u32> = items
            .iter()
            .filter_map(|item| match item {
                Ok(Element::IndirectObject { r, .. }) => Some(r.num),
                _ => None,
            })
            .collect();
        assert_eq!(good, vec![1, 2, 3, 4], "all other objects still stream");
        assert!(
            items
                .iter()
                .any(|item| matches!(item, Ok(Element::Eof { .. }))),
            "the stream runs to the end"
        );
    }

    #[tokio::test]
    async fn element_stream_is_send() {
        fn assert_send<T: Send>(value: T) -> T {
            value
        }
        let doc = AsyncDocument::from_bytes(simple_doc("send")).await.unwrap();
        let mut stream = assert_send(doc.elements(physical_opts()));
        assert!(stream.next().await.is_some());
    }
}
```

- [ ] **Step 2: Run test to verify it fails.** Add `pub mod stream;` to `src/lib.rs` first (full new file):

```rust
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
pub mod stream;

pub use backend::{Backend, BoxFuture, FileBackend, MemBackend};
pub use cache::CachedBackend;
pub use document::AsyncDocument;
pub use error::{Error, Result};
pub use stream::ElementStream;
```

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio stream::tests -- --nocapture
```

Expected failure: compile errors — `cannot find struct ElementStream` / `no method named elements found for struct AsyncDocument`.

- [ ] **Step 3: Write minimal implementation.** First add the crate-internal surface to `src/document.rs` (append to the second `impl AsyncDocument` block; also extend the elements import to `use pdfboss_core::elements::{ElementOpts, Span, XrefKind};`):

```rust
    /// Lazy element stream mirroring the sync iterator's ordering and
    /// salvage semantics. Physical elements come in file order (header,
    /// objects by offset, xref/trailer sections, startxref, eof); logical
    /// elements follow in document order (pages ascending, and within a
    /// page: fonts, images, annotations, then content ops if enabled).
    /// Nothing is fetched, parsed or decoded before it is yielded.
    pub fn elements(&self, opts: ElementOpts) -> crate::stream::ElementStream<'_> {
        crate::stream::element_stream(self, opts)
    }

    /// Fetches an in-file object together with its physical span, caching
    /// the object like [`AsyncDocument::get_object`].
    pub(crate) async fn physical_object(&self, r: ObjRef, offset: u64) -> Result<(Span, Object)> {
        let mut chain = vec![r.num];
        let (span, object) = self.parse_in_file(offset, &mut chain).await?;
        self.inner
            .objects
            .lock()
            .expect("object cache mutex")
            .insert((r.num, r.gen), Arc::new(object.clone()));
        Ok((span, object))
    }

    /// Span of the `%PDF-` header run; `None` when the file has none (the
    /// Header element is then omitted, adopted rule 1).
    pub(crate) fn header_span(&self) -> Option<Span> {
        self.inner.header_span
    }

    /// All merged xref entries (order unspecified).
    pub(crate) fn xref_entries(&self) -> Vec<(u32, XrefEntry)> {
        self.inner
            .xref
            .entries
            .iter()
            .map(|(&num, &entry)| (num, entry))
            .collect()
    }

    /// Sections in chain order (newest→oldest).
    pub(crate) fn sections(&self) -> &[SectionRecord] {
        &self.inner.sections
    }

    /// The merged trailer dictionary and the span of the newest section's
    /// trailer region, for the single Trailer element (adopted rule 4).
    pub(crate) fn merged_trailer(&self) -> (Dict, Span) {
        (
            self.inner.xref.trailer.clone(),
            self.inner.xref.trailer_span,
        )
    }

    /// The final `startxref` announcement: `(offset, span)`.
    pub(crate) fn startxref_record(&self) -> (u64, Span) {
        (self.inner.startxref.offset, self.inner.startxref.span)
    }

    /// Span of the final `%%EOF`, when one exists.
    pub(crate) fn eof_span(&self) -> Option<Span> {
        self.inner.eof_span
    }
```

Then insert the implementation into `src/stream.rs` between the module doc and the test module:

```rust
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::stream::BoxStream;
use pdfboss_core::elements::{Element, ElementOpts};
use pdfboss_core::xref::XrefEntry;
use pdfboss_core::ObjRef;

use crate::document::AsyncDocument;
use crate::error::Result;

/// Async counterpart of core's sync element iterator. `Send`, so it can
/// drive work on multi-threaded runtimes.
pub struct ElementStream<'a> {
    inner: BoxStream<'a, Result<Element>>,
}

impl<'a> futures_core::Stream for ElementStream<'a> {
    type Item = Result<Element>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

/// One unit of deferred work; producing an element (or a batch of logical
/// elements) may fetch and parse, which is exactly what laziness defers.
enum WorkItem {
    Header,
    InFile { r: ObjRef, offset: u64 },
    InStream { r: ObjRef, container: u32, index: u32 },
    Section(usize),
    Trailer,
    StartXref,
    Eof,
}

struct StreamState<'a> {
    doc: &'a AsyncDocument,
    work: VecDeque<WorkItem>,
    pending: VecDeque<Result<Element>>,
}

/// Builds the stream: the worklist is computed synchronously from state
/// the open flow already holds (no fetches); each work item is executed
/// only when the consumer polls for it.
pub(crate) fn element_stream(doc: &AsyncDocument, opts: ElementOpts) -> ElementStream<'_> {
    let state = StreamState {
        doc,
        work: build_worklist(doc, &opts),
        pending: VecDeque::new(),
    };
    ElementStream {
        inner: Box::pin(futures_util::stream::unfold(state, |mut state| async move {
            loop {
                if let Some(item) = state.pending.pop_front() {
                    return Some((item, state));
                }
                let work = state.work.pop_front()?;
                produce(&mut state, work).await;
            }
        })),
    }
}

/// Lays out the element order up front (cheap: xref entries and section
/// records are already in memory).
fn build_worklist(doc: &AsyncDocument, opts: &ElementOpts) -> VecDeque<WorkItem> {
    let mut work = VecDeque::new();
    if opts.physical {
        work.push_back(WorkItem::Header);
        let mut in_file: Vec<(u64, ObjRef)> = Vec::new();
        let mut members: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
        for (num, entry) in doc.xref_entries() {
            match entry {
                XrefEntry::Free => {}
                XrefEntry::InFile { offset, gen } => in_file.push((offset, ObjRef { num, gen })),
                XrefEntry::InStream { stream_num, index } => {
                    members.entry(stream_num).or_default().push((index, num))
                }
            }
        }
        in_file.sort_by_key(|&(offset, r)| (offset, r.num));
        for list in members.values_mut() {
            list.sort_unstable();
        }
        for (offset, r) in in_file {
            work.push_back(WorkItem::InFile { r, offset });
            if let Some(list) = members.remove(&r.num) {
                for (index, num) in list {
                    work.push_back(WorkItem::InStream {
                        r: ObjRef { num, gen: 0 },
                        container: r.num,
                        index,
                    });
                }
            }
        }
        // Members whose container has no in-file entry (broken xref) still
        // appear, after all in-file objects, ordered by container number.
        let mut leftover: Vec<(u32, Vec<(u32, u32)>)> = members.into_iter().collect();
        leftover.sort_by_key(|entry| entry.0);
        for (container, list) in leftover {
            for (index, num) in list {
                work.push_back(WorkItem::InStream {
                    r: ObjRef { num, gen: 0 },
                    container,
                    index,
                });
            }
        }
        // Sections in chain order (newest→oldest), then the single merged
        // trailer (adopted rule 4).
        for section_index in 0..doc.sections().len() {
            work.push_back(WorkItem::Section(section_index));
        }
        work.push_back(WorkItem::Trailer);
        work.push_back(WorkItem::StartXref);
        work.push_back(WorkItem::Eof);
    }
    work
}

/// Executes one work item, pushing its element(s) — or a salvage `Err` —
/// into the pending queue.
async fn produce(state: &mut StreamState<'_>, work: WorkItem) {
    let doc = state.doc;
    match work {
        WorkItem::Header => {
            if let Some(span) = doc.header_span() {
                state.pending.push_back(Ok(Element::Header {
                    version: doc.version(),
                    span,
                }));
            }
        }
        WorkItem::InFile { r, offset } => match doc.physical_object(r, offset).await {
            Ok((span, object)) => state.pending.push_back(Ok(Element::IndirectObject {
                r,
                object,
                span,
                in_objstm: None,
            })),
            Err(err) => state.pending.push_back(Err(err)),
        },
        WorkItem::InStream {
            r,
            container,
            index,
        } => match doc.objstm_cache(container).await {
            Ok(cache) => {
                let member = cache.member_span(index).and_then(|member_span| {
                    cache.object(index).map(|object| (member_span, object))
                });
                match member {
                    Ok((member_span, object)) => {
                        state.pending.push_back(Ok(Element::IndirectObject {
                            r,
                            object,
                            span: cache.container_span,
                            in_objstm: Some((cache.container, member_span)),
                        }))
                    }
                    Err(err) => state.pending.push_back(Err(err)),
                }
            }
            Err(err) => state.pending.push_back(Err(err)),
        },
        WorkItem::Section(index) => {
            let record = &doc.sections()[index];
            state.pending.push_back(Ok(Element::XrefSection {
                kind: record.kind,
                span: record.span,
                entries: record.entries,
            }));
        }
        WorkItem::Trailer => {
            let (dict, span) = doc.merged_trailer();
            state.pending.push_back(Ok(Element::Trailer { dict, span }));
        }
        WorkItem::StartXref => {
            let (offset, span) = doc.startxref_record();
            state.pending.push_back(Ok(Element::StartXref { offset, span }));
        }
        WorkItem::Eof => {
            if let Some(span) = doc.eof_span() {
                state.pending.push_back(Ok(Element::Eof { span }));
            }
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio stream::tests -- --nocapture
```

Expected: 4 tests pass (`physical_sequence_shape_for_a_classic_document`, `objstm_members_follow_their_container`, `broken_objects_yield_err_and_the_stream_continues`, `element_stream_is_send`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): ElementStream physical layer with salvage semantics"
```

---

### Task 12: stream.rs — logical layer and content ops

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/stream.rs`
- Test: unit tests inside `src/stream.rs`

**Interfaces:**
- Consumes: `page_record`/`page_count` (Task 10), `resolve`/`decode_stream` (Tasks 8–9), `pdfboss_core::content::parse_content`, `Lexer`/`Token`.
- Produces: the complete `ElementStream` semantics (logical elements per adopted rules 7–8); no new public signatures.

**Steps:**

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `src/stream.rs` (extend the test imports with `use pdfboss_core::elements::Span;` and `use pdfboss_testkit::{multi_page_doc, PdfBuilder};`):

```rust
    #[tokio::test]
    async fn logical_layer_lists_pages_fonts_images_annotations() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Im0 7 0 R >> >> \
             /Contents 4 0 R /Annots [8 0 R] >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (pic) Tj ET");
        b.object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.stream(
            7,
            "/Type /XObject /Subtype /Image /Width 2 /Height 3 \
             /ColorSpace /DeviceGray /BitsPerComponent 8",
            &[0u8; 6],
        );
        b.object(8, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>");
        let doc = AsyncDocument::from_bytes(b.build(1)).await.unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: false,
        };
        let elements: Vec<Element> = collect(&doc, opts)
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        assert!(matches!(
            elements[0],
            Element::Page { index: 0, r } if r.num == 3
        ));
        assert!(matches!(
            &elements[1],
            Element::Font { page: Some(0), r, subtype, base_font: Some(base) }
                if r.num == 5 && subtype.0 == "Type1" && base.0 == "Helvetica"
        ));
        assert!(matches!(
            &elements[2],
            Element::Image { page: Some(0), r, width: 2, height: 3 } if r.num == 7
        ));
        assert!(matches!(
            &elements[3],
            Element::Annotation { page: 0, r, subtype }
                if r.num == 8 && subtype.0 == "Link"
        ));
        assert_eq!(elements.len(), 4);
    }

    #[tokio::test]
    async fn content_ops_spans_reslice_to_the_same_op() {
        let doc = AsyncDocument::from_bytes(simple_doc("ops")).await.unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: true,
        };
        let items = collect(&doc, opts).await;
        // Recompute the decoded content the same way the sync page API does.
        let sync_doc = pdfboss_core::Document::load(simple_doc("ops")).unwrap();
        let decoded = sync_doc.page(0).unwrap().content(&sync_doc).unwrap();
        let ops: Vec<(pdfboss_core::content::Op, Span)> = items
            .iter()
            .filter_map(|item| match item {
                Ok(Element::ContentOp {
                    op, span_in_content, ..
                }) => Some((op.clone(), *span_in_content)),
                _ => None,
            })
            .collect();
        assert!(!ops.is_empty());
        // The streamed op list matches a straight parse of the content.
        let expected = pdfboss_core::content::parse_content(&decoded).unwrap();
        let streamed: Vec<pdfboss_core::content::Op> =
            ops.iter().map(|entry| entry.0.clone()).collect();
        assert_eq!(streamed, expected);
        // Re-lexing each span yields exactly that op again.
        for (op, span) in &ops {
            let slice = &decoded[span.start as usize..span.end as usize];
            let reparsed = pdfboss_core::content::parse_content(slice).unwrap();
            assert_eq!(reparsed.len(), 1, "span {span:?} holds one op");
            assert_eq!(&reparsed[0], op);
        }
    }

    #[tokio::test]
    async fn pages_filter_restricts_the_logical_layer() {
        let doc = AsyncDocument::from_bytes(multi_page_doc(&["a", "b", "c"]))
            .await
            .unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: Some(vec![1]),
            content_ops: false,
        };
        let elements: Vec<Element> = collect(&doc, opts)
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let page_indices: Vec<usize> = elements
            .iter()
            .filter_map(|el| match el {
                Element::Page { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(page_indices, vec![1]);
        assert!(elements.iter().all(|el| match el {
            Element::Font { page, .. } => *page == Some(1),
            _ => true,
        }));
    }
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio stream::tests::logical_layer -- --nocapture
```

Expected failure: `logical_layer_lists_pages_fonts_images_annotations` fails at `elements[0]` (index out of bounds or shape mismatch — the logical layer produces nothing yet). The other two new tests fail similarly.

- [ ] **Step 3: Write minimal implementation.** Replace the `use` block of `src/stream.rs` with its full new version:

```rust
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::stream::BoxStream;
use pdfboss_core::elements::{Element, ElementOpts, Span};
use pdfboss_core::lexer::{Lexer, Token};
use pdfboss_core::xref::XrefEntry;
use pdfboss_core::{Dict, Name, ObjRef, Object};

use crate::document::{AsyncDocument, PageRecord};
use crate::error::{Error, Result};
```

Replace `WorkItem` with its full new version:

```rust
/// One unit of deferred work; producing an element (or a batch of logical
/// elements) may fetch and parse, which is exactly what laziness defers.
enum WorkItem {
    Header,
    InFile { r: ObjRef, offset: u64 },
    InStream { r: ObjRef, container: u32, index: u32 },
    Section(usize),
    Trailer,
    StartXref,
    Eof,
    Page(usize),
    PageResources(usize),
    PageContentOps(usize),
}
```

Replace `build_worklist` with its full new version (the physical half is unchanged; the logical half is appended):

```rust
/// Lays out the element order up front (cheap: xref entries, section
/// records and the page index are already in memory).
fn build_worklist(doc: &AsyncDocument, opts: &ElementOpts) -> VecDeque<WorkItem> {
    let mut work = VecDeque::new();
    if opts.physical {
        work.push_back(WorkItem::Header);
        let mut in_file: Vec<(u64, ObjRef)> = Vec::new();
        let mut members: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
        for (num, entry) in doc.xref_entries() {
            match entry {
                XrefEntry::Free => {}
                XrefEntry::InFile { offset, gen } => in_file.push((offset, ObjRef { num, gen })),
                XrefEntry::InStream { stream_num, index } => {
                    members.entry(stream_num).or_default().push((index, num))
                }
            }
        }
        in_file.sort_by_key(|&(offset, r)| (offset, r.num));
        for list in members.values_mut() {
            list.sort_unstable();
        }
        for (offset, r) in in_file {
            work.push_back(WorkItem::InFile { r, offset });
            if let Some(list) = members.remove(&r.num) {
                for (index, num) in list {
                    work.push_back(WorkItem::InStream {
                        r: ObjRef { num, gen: 0 },
                        container: r.num,
                        index,
                    });
                }
            }
        }
        // Members whose container has no in-file entry (broken xref) still
        // appear, after all in-file objects, ordered by container number.
        let mut leftover: Vec<(u32, Vec<(u32, u32)>)> = members.into_iter().collect();
        leftover.sort_by_key(|entry| entry.0);
        for (container, list) in leftover {
            for (index, num) in list {
                work.push_back(WorkItem::InStream {
                    r: ObjRef { num, gen: 0 },
                    container,
                    index,
                });
            }
        }
        // Sections in chain order (newest→oldest), then the single merged
        // trailer (adopted rule 4).
        for section_index in 0..doc.sections().len() {
            work.push_back(WorkItem::Section(section_index));
        }
        work.push_back(WorkItem::Trailer);
        work.push_back(WorkItem::StartXref);
        work.push_back(WorkItem::Eof);
    }
    if opts.logical {
        for index in 0..doc.page_count() {
            if let Some(filter) = &opts.pages {
                if !filter.contains(&index) {
                    continue;
                }
            }
            work.push_back(WorkItem::Page(index));
            work.push_back(WorkItem::PageResources(index));
            if opts.content_ops {
                work.push_back(WorkItem::PageContentOps(index));
            }
        }
    }
    work
}
```

Replace `produce` with its full new version (the physical arms are unchanged from Task 11; the three logical arms are appended):

```rust
/// Executes one work item, pushing its element(s) — or a salvage `Err` —
/// into the pending queue.
async fn produce(state: &mut StreamState<'_>, work: WorkItem) {
    let doc = state.doc;
    match work {
        WorkItem::Header => {
            if let Some(span) = doc.header_span() {
                state.pending.push_back(Ok(Element::Header {
                    version: doc.version(),
                    span,
                }));
            }
        }
        WorkItem::InFile { r, offset } => match doc.physical_object(r, offset).await {
            Ok((span, object)) => state.pending.push_back(Ok(Element::IndirectObject {
                r,
                object,
                span,
                in_objstm: None,
            })),
            Err(err) => state.pending.push_back(Err(err)),
        },
        WorkItem::InStream {
            r,
            container,
            index,
        } => match doc.objstm_cache(container).await {
            Ok(cache) => {
                let member = cache.member_span(index).and_then(|member_span| {
                    cache.object(index).map(|object| (member_span, object))
                });
                match member {
                    Ok((member_span, object)) => {
                        state.pending.push_back(Ok(Element::IndirectObject {
                            r,
                            object,
                            span: cache.container_span,
                            in_objstm: Some((cache.container, member_span)),
                        }))
                    }
                    Err(err) => state.pending.push_back(Err(err)),
                }
            }
            Err(err) => state.pending.push_back(Err(err)),
        },
        WorkItem::Section(index) => {
            let record = &doc.sections()[index];
            state.pending.push_back(Ok(Element::XrefSection {
                kind: record.kind,
                span: record.span,
                entries: record.entries,
            }));
        }
        WorkItem::Trailer => {
            let (dict, span) = doc.merged_trailer();
            state.pending.push_back(Ok(Element::Trailer { dict, span }));
        }
        WorkItem::StartXref => {
            let (offset, span) = doc.startxref_record();
            state.pending.push_back(Ok(Element::StartXref { offset, span }));
        }
        WorkItem::Eof => {
            if let Some(span) = doc.eof_span() {
                state.pending.push_back(Ok(Element::Eof { span }));
            }
        }
        WorkItem::Page(index) => {
            if let Some(record) = doc.page_record(index) {
                if let Some(r) = record.r {
                    state.pending.push_back(Ok(Element::Page { index, r }));
                }
            }
        }
        WorkItem::PageResources(index) => logical_resources(state, index).await,
        WorkItem::PageContentOps(index) => content_ops(state, index).await,
    }
}
```

Append the logical-layer helpers to `src/stream.rs`:

```rust
/// Produces a page's fonts, images and annotations (in that order; fonts
/// and images sorted by resource key name, annotations in `/Annots`
/// order — adopted rule 7). Only entries that are indirect references
/// yield elements; a font or annotation missing `/Subtype` still yields
/// its element with an empty name (lenient, pinned by the core iterator);
/// other shape problems skip; fetch failures push `Err` (salvage).
async fn logical_resources(state: &mut StreamState<'_>, page: usize) {
    let doc = state.doc;
    let Some(record) = doc.page_record(page) else {
        return;
    };
    for value in sorted_dict_values(record.resources.get_dict("Font")) {
        let Some(r) = value.as_ref() else { continue };
        match doc.resolve(&value).await {
            Ok(resolved) => {
                let Some(dict) = resolved.as_dict() else { continue };
                let subtype = dict
                    .get_name("Subtype")
                    .cloned()
                    .unwrap_or_else(|| Name(String::new()));
                let base_font = dict.get_name("BaseFont").cloned();
                state.pending.push_back(Ok(Element::Font {
                    page: Some(page),
                    r,
                    subtype,
                    base_font,
                }));
            }
            Err(err) => state.pending.push_back(Err(err)),
        }
    }
    for value in sorted_dict_values(record.resources.get_dict("XObject")) {
        let Some(r) = value.as_ref() else { continue };
        match doc.resolve(&value).await {
            Ok(resolved) => {
                let Some(dict) = resolved.as_dict() else { continue };
                if dict.get_name("Subtype").map(|n| n.0.as_str()) != Some("Image") {
                    continue; // form XObjects are not image elements
                }
                let width = dict_u32(doc, dict, "Width").await;
                let height = dict_u32(doc, dict, "Height").await;
                state.pending.push_back(Ok(Element::Image {
                    page: Some(page),
                    r,
                    width,
                    height,
                }));
            }
            Err(err) => state.pending.push_back(Err(err)),
        }
    }
    let annotations = match record.dict.get("Annots") {
        Some(value) => match doc.resolve(value).await {
            Ok(Object::Array(items)) => items,
            Ok(_) => Vec::new(),
            Err(err) => {
                state.pending.push_back(Err(err));
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    for item in annotations {
        let Some(r) = item.as_ref() else { continue };
        match doc.resolve(&item).await {
            Ok(resolved) => {
                let Some(dict) = resolved.as_dict() else { continue };
                let subtype = dict
                    .get_name("Subtype")
                    .cloned()
                    .unwrap_or_else(|| Name(String::new()));
                state
                    .pending
                    .push_back(Ok(Element::Annotation { page, r, subtype }));
            }
            Err(err) => state.pending.push_back(Err(err)),
        }
    }
}

/// Values of an optional dictionary, sorted by key name (deterministic
/// logical ordering — adopted rule 7).
fn sorted_dict_values(dict: Option<&Dict>) -> Vec<Object> {
    let Some(dict) = dict else {
        return Vec::new();
    };
    let mut entries: Vec<(String, Object)> = dict
        .iter()
        .map(|(key, value)| (key.0.clone(), value.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|entry| entry.1).collect()
}

/// Resolves `dict[key]` to a `u32`, defaulting to 0 when missing or
/// invalid (adopted rule 7).
async fn dict_u32(doc: &AsyncDocument, dict: &Dict, key: &str) -> u32 {
    let Some(value) = dict.get(key) else { return 0 };
    match doc.resolve(value).await {
        Ok(resolved) => resolved
            .as_int()
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// Produces a page's content operators with their byte ranges within the
/// decoded, concatenated content stream (adopted rule 8).
async fn content_ops(state: &mut StreamState<'_>, page: usize) {
    let doc = state.doc;
    let Some(record) = doc.page_record(page) else {
        return;
    };
    let decoded = match page_content(doc, &record).await {
        Ok(decoded) => decoded,
        Err(err) => {
            state.pending.push_back(Err(err));
            return;
        }
    };
    let (spans, terminal) = op_spans(&decoded);
    for span in spans {
        let slice = &decoded[span.start as usize..span.end as usize];
        match pdfboss_core::content::parse_content(slice) {
            Ok(ops) => {
                if let Some(op) = ops.into_iter().next() {
                    state.pending.push_back(Ok(Element::ContentOp {
                        page,
                        op,
                        span_in_content: span,
                    }));
                }
                // Operators the core parser skips as unknown yield no
                // element.
            }
            Err(err) => state.pending.push_back(Err(Error::Core(err))),
        }
    }
    if let Some(err) = terminal {
        state.pending.push_back(Err(Error::Core(err)));
    }
}

/// The page's decoded content: the `/Contents` stream, or all streams of a
/// `/Contents` array decoded and joined with `b"\n"`, mirroring the sync
/// page API. A missing `/Contents` yields empty content (lenient).
async fn page_content(doc: &AsyncDocument, record: &PageRecord) -> Result<Vec<u8>> {
    let Some(contents) = record.dict.get("Contents") else {
        return Ok(Vec::new());
    };
    match doc.resolve(contents).await? {
        Object::Stream(ref s) => doc.decode_stream(s).await,
        Object::Array(items) => {
            let mut out = Vec::new();
            let mut first = true;
            for item in &items {
                let part = doc.resolve(item).await?;
                let Some(stream) = part.as_stream() else {
                    continue; // non-stream entries are skipped (lenient)
                };
                if !first {
                    out.push(b'\n');
                }
                out.extend_from_slice(&doc.decode_stream(stream).await?);
                first = false;
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// Splits a decoded content stream into per-operator byte ranges: each op
/// runs from the first byte of its first operand token to the byte after
/// its operator keyword; inline images run through `EI`. Returns the spans
/// found plus the lexer error that ended the scan, if any. Trailing
/// operands without an operator are dropped, matching the core content
/// parser.
fn op_spans(data: &[u8]) -> (Vec<Span>, Option<pdfboss_core::Error>) {
    let mut lexer = Lexer::new(data);
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    loop {
        lexer.skip_whitespace_and_comments();
        let token_start = lexer.pos();
        let token = match lexer.next_token() {
            Ok(token) => token,
            Err(err) => return (spans, Some(err)),
        };
        match token {
            Token::Eof => return (spans, None),
            Token::Keyword(keyword) => match keyword.as_slice() {
                // true/false/null are operands, not operators.
                b"true" | b"false" | b"null" => {
                    start.get_or_insert(token_start);
                }
                b"BI" => {
                    let begin = start.take().unwrap_or(token_start);
                    match inline_image_end(data, lexer.pos()) {
                        Some(end) => {
                            spans.push(Span {
                                start: begin as u64,
                                end: end as u64,
                            });
                            lexer.seek(end);
                        }
                        None => {
                            // Unterminated inline image: it takes the rest
                            // of the stream.
                            spans.push(Span {
                                start: begin as u64,
                                end: data.len() as u64,
                            });
                            return (spans, None);
                        }
                    }
                }
                _ => {
                    let begin = start.take().unwrap_or(token_start);
                    spans.push(Span {
                        start: begin as u64,
                        end: lexer.pos() as u64,
                    });
                }
            },
            _ => {
                start.get_or_insert(token_start);
            }
        }
    }
}

/// Position just past the `EI` that ends an inline image whose dictionary
/// starts at `from` (right after `BI`). Image data runs from after `ID`
/// plus one whitespace byte to `EI` at a token boundary, or past the
/// declared `/L`ength when present, which is trusted (ISO 32000 §8.9.7).
fn inline_image_end(data: &[u8], from: usize) -> Option<usize> {
    let mut lexer = Lexer::at(data, from);
    let mut declared_length: Option<usize> = None;
    let mut awaiting_length_value = false;
    loop {
        match lexer.next_token().ok()? {
            Token::Keyword(ref keyword) if keyword.as_slice() == b"ID" => break,
            Token::Name(name) => {
                awaiting_length_value = name.0 == "L" || name.0 == "Length";
            }
            Token::Int(value) => {
                if awaiting_length_value && value >= 0 {
                    declared_length = Some(value as usize);
                }
                awaiting_length_value = false;
            }
            Token::Eof => return None,
            _ => awaiting_length_value = false,
        }
    }
    let data_start = (lexer.pos() + 1).min(data.len()); // one whitespace byte after ID
    let search_from = data_start
        .saturating_add(declared_length.unwrap_or(0))
        .min(data.len());
    let mut candidate = search_from;
    while candidate + 2 <= data.len() {
        let boundary_before = candidate == 0 || is_pdf_space(data[candidate - 1]);
        let boundary_after = candidate + 2 == data.len() || is_token_boundary(data[candidate + 2]);
        if boundary_before && boundary_after && &data[candidate..candidate + 2] == b"EI" {
            return Some(candidate + 2);
        }
        candidate += 1;
    }
    None
}

/// PDF whitespace (ISO 32000 Table 1).
fn is_pdf_space(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

/// True for bytes that end a token: PDF whitespace or a delimiter.
fn is_token_boundary(byte: u8) -> bool {
    is_pdf_space(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio stream::tests -- --nocapture
```

Expected: 7 tests pass (the 4 from Task 11 plus `logical_layer_lists_pages_fonts_images_annotations`, `content_ops_spans_reslice_to_the_same_op`, `pages_filter_restricts_the_logical_layer`).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): ElementStream logical layer with per-op content spans"
```

---

### Task 13: integration tests — RecordingBackend, fetch budget, full parity, error injection

**Files:**
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/tests/common/mod.rs`
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/tests/budget.rs`
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/tests/parity.rs`
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/tests/errors.rs`
- Test: the three new integration test binaries

**Interfaces:**
- Consumes: the full public API (`AsyncDocument`, `Backend`, `BoxFuture`, `MemBackend`, `Error`, `elements`), `Document::elements` from plan 01, testkit fixtures.
- Produces: `RecordingBackend` + `ReadLog` in `tests/common` (reused by nothing else — test-only support), and the spec's three test guarantees: fetch budget, parity, error injection.

**Steps:**

- [ ] **Step 1: Write the failing test.** Create `tests/common/mod.rs`:

```rust
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
```

Create `tests/budget.rs`:

```rust
//! The huge-file guarantee: opening a multi-megabyte document and fetching
//! one object reads far less than the file — nothing ever reads it whole.

mod common;

use common::RecordingBackend;
use pdfboss_aio::{AsyncDocument, MemBackend};
use pdfboss_core::ObjRef;
use pdfboss_testkit::PdfBuilder;

fn multi_megabyte_doc() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
    );
    b.stream(4, "", b"BT (needle) Tj ET");
    // Multi-megabyte ballast between the useful objects and the xref.
    b.stream(9, "", &vec![b'x'; 3 * 1024 * 1024]);
    b.build(1)
}

#[tokio::test]
async fn opening_and_fetching_one_object_reads_less_than_64_kib() {
    let data = multi_megabyte_doc();
    assert!(data.len() > 3 * 1024 * 1024, "fixture is multi-megabyte");
    let (backend, log) = RecordingBackend::new(MemBackend::from(data));
    let doc = AsyncDocument::with_backend(backend).await.unwrap();
    let object = doc.get_object(ObjRef { num: 4, gen: 0 }).await.unwrap();
    assert!(object.as_stream().is_some());
    assert!(
        log.total_bytes() < 64 * 1024,
        "read {} bytes total; the budget is 64 KiB",
        log.total_bytes()
    );
    assert!(log.read_calls() > 0);
}
```

Create `tests/parity.rs`:

```rust
//! Byte-identical parity between the async and sync documents: objects,
//! streams, metadata, version, page count, and the full element sequence.

use futures_util::StreamExt;
use pdfboss_aio::AsyncDocument;
use pdfboss_core::elements::ElementOpts;
use pdfboss_core::{Document, ObjRef};
use pdfboss_testkit::{multi_page_doc, objstm_payload, simple_doc, PdfBuilder};

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let mut fixtures = vec![
        ("simple", simple_doc("parity")),
        ("multi_page", multi_page_doc(&["alpha", "beta", "gamma"])),
    ];
    let (dict, payload) = objstm_payload(&[
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ]);
    let mut b = PdfBuilder::new();
    b.stream(6, &dict, &payload);
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (compressed) Tj ET");
    fixtures.push(("objstm", b.build_xref_stream(1)));
    fixtures
}

/// Debug digest of one side's element sequence. `Err` items collapse to
/// "ERR": the two sides use different error types, and parity is about
/// what streams, not message text.
fn digest_sync(doc: &Document, opts: ElementOpts) -> Vec<String> {
    doc.elements(opts)
        .map(|item| match item {
            Ok(element) => format!("{element:?}"),
            Err(_) => "ERR".to_string(),
        })
        .collect()
}

async fn digest_async(doc: &AsyncDocument, opts: ElementOpts) -> Vec<String> {
    let mut stream = doc.elements(opts);
    let mut digest = Vec::new();
    while let Some(item) = stream.next().await {
        digest.push(match item {
            Ok(element) => format!("{element:?}"),
            Err(_) => "ERR".to_string(),
        });
    }
    digest
}

#[tokio::test]
async fn documents_agree_on_objects_streams_metadata_and_pages() {
    for (name, data) in fixtures() {
        let sync_doc = Document::load(data.clone()).unwrap();
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        assert_eq!(doc.version(), sync_doc.version(), "{name}: version");
        assert_eq!(doc.page_count(), sync_doc.page_count(), "{name}: pages");
        assert_eq!(
            doc.metadata().await.unwrap(),
            sync_doc.metadata(),
            "{name}: metadata"
        );
        for num in 1..=10u32 {
            let r = ObjRef { num, gen: 0 };
            match sync_doc.get(r) {
                Ok(expected) => {
                    let object = doc.get_object(r).await.unwrap();
                    assert_eq!(object, expected, "{name}: object {num}");
                    if let Some(stream) = object.as_stream() {
                        assert_eq!(
                            doc.decode_stream(stream).await.unwrap(),
                            sync_doc.stream_data(stream).unwrap(),
                            "{name}: stream {num}"
                        );
                    }
                }
                Err(_) => assert!(
                    doc.get_object(r).await.is_err(),
                    "{name}: object {num} must fail on both sides"
                ),
            }
        }
    }
}

#[tokio::test]
async fn full_element_sequences_are_identical() {
    let all = ElementOpts {
        physical: true,
        logical: true,
        pages: None,
        content_ops: true,
    };
    for (name, data) in fixtures() {
        let sync_doc = Document::load(data.clone()).unwrap();
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        for opts in [ElementOpts::default(), all.clone()] {
            let expected = digest_sync(&sync_doc, opts.clone());
            let streamed = digest_async(&doc, opts.clone()).await;
            assert_eq!(streamed, expected, "{name}: element sequence ({opts:?})");
        }
    }
}
```

Create `tests/errors.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails.** These tests are written against the finished API, so on a correct implementation of Tasks 1–12 they pass immediately; the "failing" check here is that they compile and actually run (a failure at this point is a real bug in an earlier task — debug it there, do not weaken the test):

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio --test budget --test parity --test errors -- --nocapture
```

Expected: 5 tests run. If `full_element_sequences_are_identical` fails, the mismatch is between plan 01's core iterator and the adopted span/ordering rules at the top of this plan — align the aio side (`document.rs` span computation, `stream.rs` ordering) to core's actual output, since core is the reference implementation.

- [ ] **Step 3: Write minimal implementation.** No production code is expected to change here. If Step 2 exposed a divergence, fix it in `src/document.rs` / `src/stream.rs` (span boundaries, ordering) until the digests match, keeping every earlier unit test green.

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio -- --nocapture
```

Expected: the whole crate is green — unit tests plus `opening_and_fetching_one_object_reads_less_than_64_kib`, `documents_agree_on_objects_streams_metadata_and_pages`, `full_element_sequences_are_identical`, `truncated_source_reports_truncated_read`, `failing_transport_surfaces_as_io_error`.

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "test(aio): fetch budget, sync parity and error-injection suites"
```

---

### Task 14: HTTP backend behind the `http` feature

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/backend.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/document.rs`
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/src/lib.rs`
- Create: `/Users/mohamed.tahrioui/private/pdfboss/crates/pdfboss-aio/tests/http.rs`
- Test: unit test in `src/backend.rs` (feature-gated) + `tests/http.rs`

**Interfaces:**
- Consumes: `Backend`/`BoxFuture` (Task 2), `TransportMarker` and `From<io::Error> for Error` (Task 1), `CachedBackend` (Task 4), `from_arc` (Task 7), reqwest 0.12.
- Produces (public API pinned by the spec; consumed by plans 03/04/05):

```rust
#[cfg(feature = "http")]
pub struct HttpBackend { /* reqwest::Client + Url + cached length */ }
#[cfg(feature = "http")]
impl HttpBackend { pub async fn new(url: impl reqwest::IntoUrl) -> Result<HttpBackend>; }
#[cfg(feature = "http")]
impl Backend for HttpBackend;
impl AsyncDocument {
    #[cfg(feature = "http")]
    pub async fn open_url(url: impl reqwest::IntoUrl) -> Result<AsyncDocument>;
}
```

**Steps:**

- [ ] **Step 1: Write the failing test.** Append to the `tests` module in `src/backend.rs`:

```rust
    #[cfg(feature = "http")]
    #[test]
    fn range_refusal_marker_round_trips_through_io_error() {
        let refused = http_io_error(crate::error::TransportMarker::RangeUnsupported);
        assert!(matches!(
            crate::Error::from(refused),
            crate::Error::RangeUnsupported
        ));
        let failed = http_io_error(crate::error::TransportMarker::Http {
            status: Some(503),
            msg: "unavailable".to_string(),
        });
        assert!(matches!(
            crate::Error::from(failed),
            crate::Error::Http { status: Some(503), .. }
        ));
    }
```

Create `tests/http.rs`:

```rust
#![cfg(feature = "http")]

//! The HTTP backend against a local mock server: a Range-honoring server
//! yields a working document; a Range-refusing server (200 with the full
//! body) yields `Error::RangeUnsupported`.

use std::net::SocketAddr;

use pdfboss_aio::{AsyncDocument, Error};
use pdfboss_testkit::simple_doc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Serves `data` from a minimal hand-rolled HTTP/1.1 responder.
/// `honor_range` selects whether GETs with a Range header receive 206
/// slices or the full 200 body.
async fn spawn_server(data: Vec<u8>, honor_range: bool) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((socket, peer)) = listener.accept().await else {
                break;
            };
            let payload = data.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_connection(socket, payload, honor_range).await {
                    eprintln!("mock server, peer {peer}: {err}");
                }
            });
        }
    });
    addr
}

async fn handle_connection(
    mut socket: TcpStream,
    data: Vec<u8>,
    honor_range: bool,
) -> std::io::Result<()> {
    loop {
        let Some(head) = read_request_head(&mut socket).await? else {
            return Ok(()); // client closed the connection
        };
        let total = data.len();
        if head.starts_with("HEAD ") {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await?;
            continue;
        }
        match parse_range_header(&head).filter(|_| honor_range) {
            Some((start, end)) => {
                let end = end.min(total - 1);
                let body = &data[start..=end];
                let response = format!(
                    "HTTP/1.1 206 Partial Content\r\n\
                     Content-Range: bytes {start}-{end}/{total}\r\n\
                     Content-Length: {}\r\n\r\n",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await?;
                socket.write_all(body).await?;
            }
            None => {
                // Range ignored (or absent): full body with 200.
                let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n\r\n");
                socket.write_all(response.as_bytes()).await?;
                socket.write_all(&data).await?;
            }
        }
    }
}

/// Reads one request head (through the blank line); `None` on a cleanly
/// closed connection.
async fn read_request_head(socket: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let count = socket.read(&mut byte).await?;
        if count == 0 {
            return Ok(if head.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&head).into_owned())
            });
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(Some(String::from_utf8_lossy(&head).into_owned()));
        }
    }
}

/// Extracts `Range: bytes=a-b` from a request head.
fn parse_range_header(head: &str) -> Option<(usize, usize)> {
    let line = head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))?;
    let spec = line.split('=').nth(1)?.trim();
    let (start, end) = spec.split_once('-')?;
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?))
}

#[tokio::test]
async fn open_url_works_against_a_range_honoring_server() {
    let data = simple_doc("remote");
    let sync_doc = pdfboss_core::Document::load(data.clone()).unwrap();
    let addr = spawn_server(data, true).await;
    let doc = AsyncDocument::open_url(format!("http://{addr}/remote.pdf"))
        .await
        .unwrap();
    assert_eq!(doc.version(), sync_doc.version());
    assert_eq!(doc.page_count(), sync_doc.page_count());
    assert_eq!(doc.metadata().await.unwrap(), sync_doc.metadata());
}

#[tokio::test]
async fn range_refusing_server_yields_range_unsupported() {
    let data = simple_doc("no ranges");
    let addr = spawn_server(data, false).await;
    match AsyncDocument::open_url(format!("http://{addr}/no-ranges.pdf")).await {
        Err(Error::RangeUnsupported) => {}
        Err(other) => panic!("expected RangeUnsupported, got {other:?}"),
        Ok(doc) => panic!(
            "expected failure, opened a document with {} pages",
            doc.page_count()
        ),
    }
}
```

- [ ] **Step 2: Run test to verify it fails.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio --features http -- --nocapture
```

Expected failure: compile errors — `cannot find function http_io_error`, `cannot find struct HttpBackend`, `no function or associated item named open_url`.

- [ ] **Step 3: Write minimal implementation.** Append to `src/backend.rs` (below `FileBackend`'s impl, above the test module):

```rust
/// A byte source over HTTP: length via `HEAD`/`Content-Length`, reads via
/// `Range: bytes=` requests. A server that ignores Range (answers 200
/// with the full body instead of 206) yields
/// [`crate::Error::RangeUnsupported`].
#[cfg(feature = "http")]
pub struct HttpBackend {
    client: reqwest::Client,
    url: reqwest::Url,
    len: u64,
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
        Ok(HttpBackend { client, url, len })
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
            let last = (offset + buf.len() as u64 - 1).min(self.len - 1);
            let response = self
                .client
                .get(self.url.clone())
                .header(reqwest::header::RANGE, format!("bytes={offset}-{last}"))
                .send()
                .await
                .map_err(|err| {
                    http_io_error(crate::error::TransportMarker::Http {
                        status: err.status().map(|status| status.as_u16()),
                        msg: format!("GET {} range {offset}-{last}: {err}", self.url),
                    })
                })?;
            match response.status().as_u16() {
                206 => {}
                200 => {
                    // The server ignored the Range header and answered
                    // with the whole body: range fetching cannot work.
                    return Err(http_io_error(
                        crate::error::TransportMarker::RangeUnsupported,
                    ));
                }
                status => {
                    return Err(http_io_error(crate::error::TransportMarker::Http {
                        status: Some(status),
                        msg: format!("GET {} range {offset}-{last} failed", self.url),
                    }));
                }
            }
            let body = response.bytes().await.map_err(|err| {
                http_io_error(crate::error::TransportMarker::Http {
                    status: None,
                    msg: format!("GET {} range {offset}-{last}: {err}", self.url),
                })
            })?;
            let count = buf.len().min(body.len());
            buf[..count].copy_from_slice(&body[..count]);
            Ok(count)
        })
    }
}
```

Add to the first `impl AsyncDocument` block in `src/document.rs` (next to `open`):

```rust
    /// Opens a remote document over HTTP range requests, wrapped in a
    /// [`CachedBackend`] with default capacity.
    #[cfg(feature = "http")]
    pub async fn open_url(url: impl reqwest::IntoUrl) -> Result<AsyncDocument> {
        let backend = crate::backend::HttpBackend::new(url).await?;
        AsyncDocument::from_arc(Arc::new(CachedBackend::new(backend))).await
    }
```

Update `src/lib.rs` to its full, final version (plan 03 consumes `AsyncDocument` and `ElementStream` from the crate root — both stay re-exported):

```rust
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
pub mod stream;

#[cfg(feature = "http")]
pub use backend::HttpBackend;
pub use backend::{Backend, BoxFuture, FileBackend, MemBackend};
pub use cache::CachedBackend;
pub use document::AsyncDocument;
pub use error::{Error, Result};
pub use stream::ElementStream;
```

- [ ] **Step 4: Run test to verify it passes.**

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio --features http -- --nocapture
```

Expected: everything green, including `backend::tests::range_refusal_marker_round_trips_through_io_error`, `open_url_works_against_a_range_honoring_server`, `range_refusing_server_yields_range_unsupported`. Also verify the default feature set still builds without reqwest:

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio
```

Expected: green, with the http-gated tests absent.

- [ ] **Step 5: Commit.**

```bash
git add crates/pdfboss-aio && git commit -m "feat(aio): HTTP range backend and open_url behind the http feature"
```

---

### Task 15: CI wiring and full-workspace verification

**Files:**
- Modify: `/Users/mohamed.tahrioui/private/pdfboss/.github/workflows/ci.yaml`
- Test: the full workspace gate commands below

**Interfaces:**
- Consumes: the finished crate (Tasks 1–14).
- Produces: CI coverage for `pdfboss-aio` including its `http` feature.

The existing `ci.yaml` runs `cargo fmt --all`, `cargo clippy --workspace --all-targets`, `cargo test --workspace` and `cargo doc --workspace --no-deps` — all of which pick up a new workspace member automatically, so no members-list change is needed in the workflow. What the workflow does NOT cover is the `http` feature (nothing enables it), so the clippy and test jobs each gain one targeted step. `--all-features` is applied only to `pdfboss-aio`, not the workspace, so other crates' opt-in features (e.g. the render crate's bundled-font feature) stay out of CI.

Release-please needs no change (adopted rule 10): the crate inherits the workspace version from the root `Cargo.toml`'s `# x-release-please-version` marker, which the config's root package already maintains via `extra-files`.

**Steps:**

- [ ] **Step 1: Write the failing test.** The "test" is the CI configuration gap: confirm the current workflow never builds the `http` feature.

```bash
grep -n "all-features\|pdfboss-aio" /Users/mohamed.tahrioui/private/pdfboss/.github/workflows/ci.yaml
```

- [ ] **Step 2: Run test to verify it fails.** Expected: no matches (exit status 1) — the feature is uncovered.

- [ ] **Step 3: Write minimal implementation.** In `.github/workflows/ci.yaml`, the `clippy` and `test` jobs become (full new versions of both jobs; `fmt` and `doc` stay unchanged):

```yaml
  clippy:
    name: clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo clippy -p pdfboss-aio --all-targets --all-features -- -D warnings

  test:
    name: test
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo test --workspace
      - run: cargo test -p pdfboss-aio --all-features
```

- [ ] **Step 4: Run test to verify it passes.** Run the full local gate exactly as CI will:

```bash
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-aio --all-targets --all-features -- -D warnings
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test --workspace
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-aio --all-features
CARGO_TARGET_DIR=$HOME/.cargo/shared-target RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Expected: every command exits 0; all existing crates' tests stay green (the sync API is untouched); the grep from Step 1 now matches both new lines.

- [ ] **Step 5: Commit.**

```bash
git add .github/workflows/ci.yaml && git commit -m "ci: cover pdfboss-aio http feature in clippy and test jobs"
```
