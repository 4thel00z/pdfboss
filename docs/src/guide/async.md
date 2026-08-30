# Async and remote PDFs over HTTP

`AsyncDocument` opens a PDF without reading the whole file. The open flow fetches only what it needs (the header, the cross-reference chain and the page tree) and every later operation fetches only the byte ranges it touches. The file backend reads windows of the file on demand; the HTTP backend turns each read into a `Range` request, so a document on a server can be paged through without downloading it. A server that ignores `Range` and answers `200` with the full body (`python3 -m http.server`, for one) still works: the first such answer is kept as the whole resource and every read is served from it, at the cost of one full download held in memory for the life of the document.

## Python

Three constructors, all coroutines. Each takes `password=` for encrypted files (see [Encrypted documents](./encryption.md)):

| Constructor | Source |
|---|---|
| `AsyncDocument.open(path)` | A local file, read in ranges |
| `AsyncDocument.open_url(url)` | An http(s) URL, fetched via `Range` requests |
| `AsyncDocument.from_bytes(data)` | Bytes already in memory |

What is sync and what is a coroutine follows from what the open flow already parsed. `page_count`, `version`, `len(doc)` and page access (`doc[i]`, `doc.page(i)` and every `AsyncPage` geometry property) are plain sync attributes: the xref chain and the page tree were parsed at open, so nothing there needs I/O. Everything that must read more of the file is a coroutine: `extract_text`, `extract_markdown`, `render_pages`, `metadata`, `get_object` on the document, and `extract_text`, `extract_markdown`, `render`, `render_reporting`, `extract_images` and `spans` on a page.

Coroutines are driven by one shared multi-thread tokio runtime behind the asyncio loop. `render_pages` fans pages across the machine's cores as tokio tasks, so the loop stays free while pages rasterize.

```python
import asyncio

import pdfboss


async def main() -> None:
    doc = await pdfboss.AsyncDocument.open("report.pdf")
    print(doc.page_count, doc.version)

    text = await doc.extract_text()
    metadata = await doc.metadata()
    png = await doc[0].render(scale=2.0)

    async for span in doc.spans(pages=[0]):
        print(span.text, span.font_name)


asyncio.run(main())
```

`doc.elements()` and `doc.spans()` return async iterators, consumed with `async for`, with the same ordering and salvage semantics as their sync twins. See [Exploring PDF internals](./explorer.md) and [Styled spans](./spans.md).

A remote document differs only in the constructor:

```python
import asyncio

import pdfboss


async def main() -> None:
    doc = await pdfboss.AsyncDocument.open_url("https://example.com/report.pdf")
    print(doc.page_count)
    print(await doc.extract_markdown())


asyncio.run(main())
```

## Rust

`pdfboss_aio::AsyncDocument` has the same constructors: `open`/`open_with_password`, `from_bytes`/`from_bytes_with_password` and, behind the crate's `http` feature, `open_url`/`open_url_with_password`. `with_backend` opens a document over any byte source you build yourself.

```rust,no_run
use pdfboss_aio::AsyncDocument;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = AsyncDocument::open_url("https://example.com/report.pdf").await?;
    println!("{} pages", doc.page_count());

    let metadata = doc.metadata().await?;
    println!("{:?}", metadata.title);
    Ok(())
}
```

### Backends

A document reads through the `Backend` trait: `len()` and `read_at(offset, buf)`, both returning boxed futures so the trait is object-safe and a document can hold `Arc<dyn Backend>`. Four implementations ship with the crate:

- `MemBackend`: bytes fully resident in memory; `from_bytes` uses it directly, with no cache.
- `FileBackend`: positioned reads (`pread`-style, no shared cursor) run on tokio's blocking thread pool, so disk I/O never stalls the async runtime. The length is captured once at open; the file is treated as immutable while the backend lives.
- `HttpBackend` (feature `http`): the length comes from a `HEAD` request's `Content-Length`; each read is a `GET` with a `Range: bytes=` header. A `200` answer where `206` was asked for means the server ignores `Range`; its body is the whole resource, so it is collected once (capped at the declared length, so a buggy or hostile server cannot balloon memory) and all reads are served from it. `on_fallback_progress` registers an observer for that one-time download (the CLI draws its stderr progress bar through it). A `206` body is likewise collected only up to the requested size.
- `CachedBackend`: a chunked LRU read cache over any backend: many small reads become few chunk-sized fetches, and hot chunks stay resident up to a byte budget. Defaults: 64 KiB chunks, 32 MiB total. Misses batch adaptively: a miss landing near the previous one doubles the batch, up to 8 MiB (and a quarter of the budget), a far jump halves it, and each miss fetches its uncached neighborhood, growing around the missed chunk until it hits resident chunks or the batch budget. Dense access over a high-latency server collapses into few large requests, whichever direction the reader walks the file, while scattered access never over-fetches.

`open` and `open_url` wrap their backend in a `CachedBackend` automatically. `from_bytes` stays uncached, and `with_backend` adds nothing, so a composition of your own is used exactly as given (`with_backend_with_password` is the same for encrypted files):

```rust,no_run
use pdfboss_aio::{AsyncDocument, CachedBackend, FileBackend};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = FileBackend::open("report.pdf").await?;
    let cached = CachedBackend::with_capacity(backend, 128 * 1024, 64 * 1024 * 1024);
    let doc = AsyncDocument::with_backend(cached).await?;
    println!("{} pages", doc.page_count());
    Ok(())
}
```

### The sync crates over an async document

The extraction and rendering crates are written sans-I/O. Each entry point is implemented as a `*_with` function generic over `pdfboss_core::AsyncObjectSource`; the sync signature is that same implementation run over an immediate, no-I/O source. `AsyncDocument` implements `AsyncObjectSource`, so `pdfboss_output::extract_text_with`, `pdfboss_output::extract_page_markdown_with`, `pdfboss_render::render_page_reporting_with` and `pdfboss_render::extract_page_images_with` all run over range-fetching reads unchanged. The document is an `Arc` handle: cloning one to hand to an entry point by value costs two atomic increments.

```rust,no_run
use pdfboss_aio::AsyncDocument;
use pdfboss_output::extract_text_with;
use pdfboss_render::extract_page_images_with;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = AsyncDocument::open("report.pdf").await?;
    println!("{} pages", doc.page_count());

    let page = doc.page(0)?;
    let oc = doc.oc_state().await;
    let text = extract_text_with(doc.clone(), &page, oc.as_ref()).await?;
    println!("{text}");

    let images = extract_page_images_with(doc.clone(), &page).await?;
    for (index, image) in images.iter().enumerate() {
        image.save_png(format!("image-{index}.png"))?;
    }
    Ok(())
}
```

The `oc` parameter carries the document's optional-content visibility (`doc.oc_state().await`); text and markdown extraction use it to exclude layers the document's default configuration turns off, exactly as the sync entry points do. Rendering takes the same state through `RenderOptions::oc`: set `opts.oc = doc.oc_state().await.map(Arc::new);` before calling `render_page_reporting_with`. Leaving it `None` renders every layer; only the sync entry points fill it from the document. What the extracted images contain (drawing order, native size, `/SMask` alpha) is described in [Extracting images](./images.md); the sync Rust and Python surfaces are in [Rust crates](../reference/rust.md) and [Python API](../reference/python.md).
