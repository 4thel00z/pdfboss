# pdfboss — element iterator, async I/O, TUI explorer, fq-style CLI — design spec (2026-07-24)

Adds four stacked capabilities on top of the existing sync core:

1. A lazy **element iterator** over everything in a PDF — physical file structure
   (with byte spans) and logical document structure — in `pdfboss-core`.
2. A new **`pdfboss-aio`** crate: fully async I/O (huge files without loading them,
   many documents concurrently, remote PDFs over HTTP range requests) built
   sans-I/O style on the existing sync parser.
3. **Python bindings** for both: sync `for` iteration on `Document`, and a new
   `AsyncDocument` usable with `await` / `async for` from asyncio.
4. A **ratatui TUI** (`pdfboss tui`) to explore a complete PDF, and an
   **fq+hexyl-style CLI** (`pdfboss q`, `pdfboss hex`, `pdfboss json`).

## Ground rules

- The cleanroom rule from the 2026-07-12 spec applies unchanged: everything is
  implemented from ISO 32000; never name any other PDF library anywhere.
  Non-PDF dependencies (tokio, ratatui, crossterm, jaq, reqwest, futures) are fine.
- `pdfboss-core` gains **zero** new dependencies. No async, no serde, no jq
  anywhere in core. The jq engine and JSON conversion live **only** in
  `pdfboss-cli`.
- The existing sync API (`Document`, `Page`, text, render) and all existing
  tests stay untouched. New capability is additive.
- Shared build cache: agents use the global `~/.cargo/shared-target`; never
  per-agent target dirs.

## Non-goals (v1)

- No PDF editing/writing from the TUI or CLI.
- No sixel/kitty graphics protocol — page previews use half-block cells only.
- No lazy jq evaluation — `pdfboss q` materializes the value tree per query.
- Encryption support is whatever `crypt.rs` already does; no changes.
- No incremental-save awareness beyond what the xref chain walk already gives.

## Workspace layout (delta)

```
crates/
  pdfboss-core/     + src/elements.rs   + src/pretty.rs (moved from cli)
  pdfboss-aio/      NEW: backend.rs, cache.rs, document.rs, stream.rs, error.rs
  pdfboss-tui/      NEW: app.rs, tree.rs, inspector.rs, hexview.rs, preview.rs,
                         search.rs, input.rs, ui.rs
  pdfboss-cli/      + src/q/ (value.rs, run.rs)  + src/hexdump.rs  + src/json.rs
                    - src/pretty.rs (moved to core)
  pdfboss-py/       + Element, ElementIter, AsyncDocument, AsyncElementIter
```

`pdfboss-tui` is a library crate exposing `pub async fn run(doc: AsyncDocument, title: String) -> Result<()>`;
`pdfboss-cli` wires it up as the `tui` subcommand. Both new crates are
publishable and versioned with the workspace (`# x-release-please-version`
handling identical to existing crates; release-please config gains the two
crates).

## pdfboss-core: elements.rs

Pure sync, no new deps. Public API:

```rust
/// Byte range in the physical file, end-exclusive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span { pub start: u64, pub end: u64 }

#[derive(Clone, Debug)]
pub enum Element {
    // ---- physical layer: file structure, always with spans ----
    Header { version: (u8, u8), span: Span },
    IndirectObject {
        r: ObjRef,
        object: Object,
        /// Span of `N G obj … endobj` in the file. For objects stored in an
        /// object stream this is the *container stream object's* span.
        span: Span,
        /// Set when the object lives inside an object stream: the container's
        /// ref and this object's byte range *within the decoded* stream.
        in_objstm: Option<(ObjRef, Span)>,
    },
    XrefSection { kind: XrefKind, span: Span, entries: usize },
    Trailer { dict: Dict, span: Span },
    StartXref { offset: u64, span: Span },
    Eof { span: Span },

    // ---- logical layer: document semantics ----
    Page { index: usize, r: ObjRef },
    Font { page: Option<usize>, r: ObjRef, subtype: Name, base_font: Option<Name> },
    Image { page: Option<usize>, r: ObjRef, width: u32, height: u32 },
    Annotation { page: usize, r: ObjRef, subtype: Name },
    ContentOp {
        page: usize,
        op: content::Op,           // existing content-stream operator type
        /// Byte range within the page's *decoded, concatenated* content stream.
        span_in_content: Span,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XrefKind { Table, Stream }

#[derive(Clone, Debug, Default)]
pub struct ElementOpts {
    pub physical: bool,            // default true
    pub logical: bool,             // default true
    pub pages: Option<Vec<usize>>, // logical layer: restrict to these pages
    pub content_ops: bool,         // default false — ops are high-volume
}
```

`Default` yields `physical: true, logical: true, pages: None, content_ops: false`.

```rust
impl Document {
    /// Lazy iteration over the document's elements. Physical elements come in
    /// file order (header, objects by offset, xref/trailer sections, eof);
    /// logical elements follow in document order (pages ascending, and within
    /// a page: fonts, images, annotations, then content ops if enabled).
    /// Nothing is parsed or decoded before it is yielded.
    pub fn elements(&self, opts: ElementOpts) -> Elements<'_>;
}

pub struct Elements<'a> { /* iterator state machine */ }
impl<'a> Iterator for Elements<'a> {
    type Item = Result<Element>;
}
```

Implementation notes:

- Physical order comes from sorting live xref entries by offset; free entries
  are skipped. Objects that fail to parse yield `Err` for that item and the
  iterator continues (salvage semantics — one bad object must not kill
  exploration).
- Spans for classic objects run from the offset in the xref to the byte after
  `endobj` (parser must report consumed length — small extension to the
  existing object parser entry point, sync and internal).
- `content_ops` reuses `content.rs`'s existing operator parser; the content
  lexer must report per-op byte offsets in the decoded stream (small
  extension, again internal).
- `pretty.rs` moves from `pdfboss-cli` to `pdfboss-core::pretty` unchanged in
  behavior (object → human-readable text). CLI re-exports/uses it; the TUI
  inspector uses it too.

## pdfboss-aio

New crate. Dependencies: `pdfboss-core`, `tokio` (rt, fs, io-util, sync),
`futures-core`/`futures-util`, `bytes`; `reqwest` behind the `http` feature
(default off).

### backend.rs

```rust
/// Random-access byte source. Object-safe: futures are boxed.
pub trait Backend: Send + Sync + 'static {
    fn len(&self) -> BoxFuture<'_, io::Result<u64>>;
    /// Read up to buf.len() bytes at `offset`. Short reads only at EOF.
    fn read_at<'a>(&'a self, offset: u64, buf: &'a mut [u8])
        -> BoxFuture<'a, io::Result<usize>>;
}

pub struct MemBackend(bytes::Bytes);          // From<Vec<u8>>, From<Bytes>
pub struct FileBackend { /* std::fs::File + spawn_blocking positioned reads */ }
impl FileBackend { pub async fn open(path: impl AsRef<Path>) -> io::Result<Self>; }

#[cfg(feature = "http")]
pub struct HttpBackend { /* reqwest::Client + url; len via HEAD/Content-Length,
                            read_at via `Range: bytes=` requests. Fails with a
                            clear error if the server ignores Range. */ }
#[cfg(feature = "http")]
impl HttpBackend { pub async fn new(url: impl IntoUrl) -> Result<Self>; }
```

### cache.rs

```rust
/// Chunked LRU read cache over any backend. Default 64 KiB chunks, 32 MiB cap.
pub struct CachedBackend<B: Backend> { /* … */ }
impl<B: Backend> CachedBackend<B> {
    pub fn new(inner: B) -> Self;
    pub fn with_capacity(inner: B, chunk_size: usize, max_bytes: usize) -> Self;
}
impl<B: Backend> Backend for CachedBackend<B> { /* … */ }
```

`AsyncDocument::open_*` constructors wrap file/http backends in `CachedBackend`
automatically; `MemBackend` is used as-is.

### document.rs

```rust
pub struct AsyncDocument { /* Arc<dyn Backend>, parsed xref chain, trailer,
                              page tree index, objstm decode cache (tokio::sync::Mutex) */ }

impl AsyncDocument {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self>;          // FileBackend
    pub async fn from_bytes(bytes: impl Into<Bytes>) -> Result<Self>;   // MemBackend
    #[cfg(feature = "http")]
    pub async fn open_url(url: impl IntoUrl) -> Result<Self>;           // HttpBackend
    pub async fn with_backend(backend: impl Backend) -> Result<Self>;

    pub fn version(&self) -> (u8, u8);
    pub fn page_count(&self) -> usize;
    pub async fn metadata(&self) -> Result<Metadata>;
    pub async fn get_object(&self, r: ObjRef) -> Result<Object>;
    pub async fn resolve(&self, obj: &Object) -> Result<Object>;
    pub async fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>>;
    pub async fn read_span(&self, span: Span) -> Result<Vec<u8>>;       // raw bytes, for hex views
    pub fn elements(&self, opts: ElementOpts) -> ElementStream<'_>;
}
```

Open flow: fetch the last 4 KiB (growing backwards if needed) → `%%EOF` /
`startxref` → walk the xref chain (`/Prev`), fetching only xref-table /
xref-stream spans → merged xref + trailer → build the page-tree index by
fetching only catalog/pages nodes. **The whole file is never read.**

`get_object`: xref offset → fetch a 2 KiB window (grow geometrically until
`endobj` or stream extent known; indirect `/Length` triggers one extra object
fetch) → sync-parse from the buffer. Object-stream containers are fetched and
decoded once, then cached; member objects parse out of the cached buffer.

Everything is `&self`; `AsyncDocument: Send + Sync + Clone` (cheap Arc clone)
so servers share one instance across tasks and hold many documents at once.

### stream.rs

```rust
pub struct ElementStream<'a> { /* async state machine mirroring core's Elements */ }
impl<'a> futures_core::Stream for ElementStream<'a> {
    type Item = Result<Element>;
}
```

Same ordering and salvage semantics as the sync iterator. Implemented with
`async-stream`-style generator or hand-rolled state machine — implementer's
choice, pinned only by the public type above being `Send`.

### error.rs

```rust
pub enum Error {
    Core(pdfboss_core::Error),
    Io(std::io::Error),
    #[cfg(feature = "http")]
    Http { status: Option<u16>, msg: String },
    RangeUnsupported,      // server ignored Range requests
    TruncatedRead { offset: u64, wanted: usize, got: usize },
}
pub type Result<T> = std::result::Result<T, Error>;
```

## pdfboss-py

Deps added: `pyo3-async-runtimes` (tokio runtime, one global multi-thread
runtime). New surface (all mirrored in `_pdfboss.pyi`):

```python
class Element:
    kind: str                      # "header" | "object" | "xref" | "trailer" |
                                   # "startxref" | "eof" | "page" | "font" |
                                   # "image" | "annotation" | "content_op"
    span: tuple[int, int] | None   # physical byte range
    ref: tuple[int, int] | None    # (num, gen) where applicable
    page: int | None               # logical elements
    def value(self) -> object: ... # lazy conversion: dict/list/str/bytes/int/float/bool/None
                                   # PDF names -> str, streams -> {"dict": ..., "length": int}

class Document:
    def elements(self, *, physical: bool = True, logical: bool = True,
                 pages: list[int] | None = None,
                 content_ops: bool = False) -> Iterator[Element]: ...

class AsyncDocument:
    @staticmethod
    async def open(path: str | os.PathLike) -> "AsyncDocument": ...
    @staticmethod
    async def open_url(url: str) -> "AsyncDocument": ...   # http feature (on for wheels)
    @staticmethod
    async def from_bytes(data: bytes) -> "AsyncDocument": ...
    def page_count(self) -> int: ...
    def version(self) -> str: ...
    async def metadata(self) -> dict[str, str]: ...
    async def get_object(self, num: int, gen: int = 0) -> object: ...
    def elements(self, *, physical: bool = True, logical: bool = True,
                 pages: list[int] | None = None,
                 content_ops: bool = False) -> AsyncIterator[Element]: ...
```

- Sync `Document.elements` iterates the core iterator; each `__next__` releases
  the GIL while parsing.
- `AsyncDocument.elements` returns an object implementing `__aiter__`/`__anext__`
  via pyo3-async-runtimes futures; the asyncio loop is never blocked.
- All errors map to the existing `PdfError`.
- Wheels build `pdfboss-aio` with `http` enabled.
- Python CI gains `pytest-asyncio`.

## pdfboss-tui

Library crate; deps: `pdfboss-aio`, `pdfboss-core`, `pdfboss-render`,
`ratatui`, `crossterm` (event-stream feature), `tokio`, `futures-util`.
Runs on a current-thread tokio runtime created by the CLI subcommand.

Layout (approved):

```
┌ Tree ──────────────┬ Inspector ────────────────────┐
│ ▾ Document          │ 12 0 obj  << /Type /Page      │
│   ▾ Pages (14)      │   /MediaBox [0 0 612 792]     │
│     ▸ Page 1        │   /Contents 13 0 R            │
│   ▸ Objects (241)   │ >>                            │
│   ▸ Xref (3 secs)   ├ Hex (obj 12: 0x1a40..0x1b02) ─┤
│   ▸ Trailer         │ 00001a40 31 32 20 30 20 6f …  │
├─────────────────────┴───────────────────────────────┤
│ /Pages/2 · obj 12 0 · [/] search  [q] quit          │
└─────────────────────────────────────────────────────┘
```

- **Tree** (left, ~35% width): Document → Pages (per page: Fonts, Images,
  Annotations, Contents) → Objects (flat, by number) → Xref sections → Trailer.
  Nodes populate lazily from `AsyncDocument.elements` on first expand.
- **Inspector** (right-top): selected element pretty-printed via core's
  `pretty`. Streams: `d` toggles raw bytes / decoded bytes / (for content
  streams) disassembled operators one per line.
- **Hex** (right-bottom): hexyl-style — `offset │ hex │ ascii`, colorized byte
  classes (null / printable / whitespace / other), showing the selection's
  span via `AsyncDocument::read_span`; scrollable within the span; PgUp/PgDn.
  For objstm members it shows the decoded container with the member's range
  highlighted.
- **Preview**: `p` swaps the inspector for a page preview — rasterize the
  selected page via `pdfboss-render` in `spawn_blocking` at fit-to-pane scale,
  paint with `▀` half-blocks (two pixels per cell via fg+bg color); spinner
  while rendering; re-renders on resize (debounced).
- **Navigation**: ↑↓/jk move, ←→/hl collapse/expand, Tab cycles pane focus,
  Enter on a reference (`N G R`) jumps to that object, Backspace pops the jump
  history, `g`/`G` top/bottom, `q`/Esc quits.
- **Search**: `/` opens an input in the status bar; incremental match over
  object numbers, dict keys, name values, and string contents (visits objects
  lazily, streaming results); `n`/`N` next/previous hit; Esc cancels.
- Event loop: `tokio::select!` over crossterm `EventStream` and background
  tasks (search, preview render). Long operations never block input.
- Errors surface as a status-bar toast, never a panic; a document whose xref
  is unusable falls back to whatever core recovery yields — physical layer
  exploration must still work when the logical tree can't be built.

## pdfboss-cli: q / hex / json / tui subcommands

Deps added to the CLI crate **only**: `jaq-core`, `jaq-std`, `serde_json`,
`base64`, plus `pdfboss-aio` and `pdfboss-tui`. Existing subcommands untouched.

### Value tree (src/q/value.rs)

The CLI converts a document into a `serde_json::Value` (never core's job):

```jsonc
{
  "header": { "version": "1.7", "_span": [0, 15], "_kind": "header" },
  "objects": {
    "12 0": { "_kind": "object", "_ref": [12, 0], "_span": [6720, 6914],
              "_objstm": null,
              "value": { "Type": "Page", "MediaBox": [0,0,612,792],
                         "Contents": {"_r": [13, 0]} } },
    "13 0": { "_kind": "object", "_ref": [13, 0], "_span": [6914, 7480],
              "value": { "_stream": { "dict": {"Length": 520, "Filter": "FlateDecode"},
                                       "length": 520 } } }
  },
  "pages": [ { "index": 0, "_ref": [3, 0], "fonts": [...], "images": [...],
               "annotations": [...] } ],
  "xref": [ { "kind": "table", "entries": 42, "_span": [7480, 8322] } ],
  "trailer": { "_span": [8322, 8419], "value": { "Root": {"_r": [1, 0]}, "Size": 42 } },
  "startxref": 7480
}
```

Conventions: metadata keys start with `_` (fq-style); indirect references
encode as `{"_r": [num, gen]}`; names as plain strings; PDF strings as UTF-8
where valid else `{"_bytes": "<base64>"}`. Stream data is omitted by default;
`--raw` embeds `{"_stream": {..., "data": "<base64 raw>"}}`, `--decode` embeds
decoded data instead. `--pages`/`--no-logical`/`--content-ops` map to
`ElementOpts`.

### pdfboss q

```
pdfboss q <file-or-url> '<jq program>' [--raw|--decode] [--hex] [-r] [--pages ..]
```

Runs the jq program via jaq over the value tree. Output: pretty JSON, colored
on a tty; `-r` raw strings. `--hex` post-processes each result: if it is an
object containing `_span` (or an array of such), print a hexyl-style dump of
those byte ranges instead of JSON — query-to-bytes. URLs use the aio HTTP
backend; local paths use `Document` sync (fast path).

### pdfboss hex

```
pdfboss hex <file-or-url> [selector] [--annotate] [--width N]
```

Selectors: `obj:12` / `obj:12,0`, `header`, `xref:0`, `trailer`,
`range:0x1A40-0x1B02`, default = whole file. hexyl-style colorized output
(offset gutter, hex, ascii; byte-class colors; color auto-disabled when not a
tty or `NO_COLOR`). `--annotate` prints labeled region boundary lines
(`── obj 12 0 ──`) inline as the dump crosses element spans.

### pdfboss json

`pdfboss json <file-or-url> [--raw|--decode] [--pages ..]` — dumps the full
value tree for piping to external tools.

### pdfboss tui

`pdfboss tui <file-or-url>` — builds the backend (file or http), constructs
`AsyncDocument`, hands off to `pdfboss_tui::run` on a current-thread runtime.

## Errors

- aio's `Error` wraps core/io/http as above; every fetch failure carries the
  offset/range it was fetching for diagnosability.
- CLI exits nonzero with a one-line message; `q` reports jq compile errors
  with position, distinct from PDF errors.
- Python: everything becomes `PdfError` (message prefixed by layer:
  `"http: …"`, `"parse: …"`).
- TUI: toasts + salvage mode as above.

## Testing strategy

- **core/elements**: for every testkit fixture, iterate physical elements and
  assert (a) spans are disjoint-or-nested and within file bounds, (b) re-parsing
  the bytes at each object span yields an equal `Object`, (c) logical elements
  agree with the page API (same fonts/images per page). Content-op spans:
  slicing the decoded stream at `span_in_content` re-lexes to the same op.
- **aio**: a `RecordingBackend` wrapper logs every `read_at`. Assertions:
  opening a fixture + fetching one object reads < 64 KiB total for a multi-MB
  fixture (huge-file guarantee); byte-identical results vs. sync `Document`
  for objects, streams, metadata, and the full element sequence. Error
  injection: truncated reads, failing ranges. HTTP backend: local
  hyper-served mock honoring/refusing Range; refusal must yield
  `RangeUnsupported`.
- **py**: pytest — sync elements on a fixture; `pytest-asyncio` for
  `AsyncDocument.open` / `async for` / `open_url` against a local range
  server; parity of sync vs async element sequences.
- **tui**: pure-logic unit tests (tree building, search matching, hex
  formatting, span highlighting) + ratatui `TestBackend` snapshot tests of
  full frames for a fixture PDF (tree render, inspector dict, hex pane,
  status bar).
- **cli**: golden tests (color stripped): `q` programs (`.objects["12 0"]`,
  `select` over kinds, `--hex` output), `hex` selectors and `--annotate`,
  `json` round-trip stability.
- CI: new crates ride the existing ci.yaml matrix; python-ci adds
  pytest-asyncio; release-please config registers `pdfboss-aio` and
  `pdfboss-tui`.

## Suggested implementation order

1. core: `elements.rs` + parser span plumbing + `pretty` move (foundation).
2. `pdfboss-aio`: backends + cache + `AsyncDocument` + `ElementStream`.
3. py: sync `elements()`, then `AsyncDocument`.
4. cli: value tree + `json` + `hex` (no jq yet), then `q` with jaq + `--hex`.
5. `pdfboss-tui`: app skeleton + tree/inspector/hex, then search, follow-refs,
   decoded views, preview.
