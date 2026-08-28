# Python API

The `pdfboss` package re-exports the compiled extension module `pdfboss._pdfboss`. Its public surface is twelve classes plus the `md` submodule; the typed stubs in [`_pdfboss.pyi`](https://github.com/4thel00z/pdfboss/blob/main/python/pdfboss/_pdfboss.pyi) are the authoritative reference for every signature and docstring. This chapter is the inventory; worked examples live in the guide chapters.

## The twelve classes

| Name | What it is |
|---|---|
| `Document` | A loaded PDF, from a path or bytes; pages by index, `extract_text`, `extract_markdown`, `render_pages`, `elements`, `spans` |
| `Page` | One page: geometry (width/height/rotation and the five boxes), `extract_text`, `extract_markdown`, `spans`, `render`, `render_reporting`, `extract_images` |
| `AsyncDocument` | The async twin of `Document`, opened from a path, bytes, or an HTTP URL via range requests; data-fetching methods are coroutines |
| `AsyncPage` | The async twin of `Page`; attributes are synchronous, extraction and rendering are coroutines |
| `Element` | One physical or logical element of a PDF (`kind`, `span`, `ref`, `page`, lazy `value()`), yielded by `elements` |
| `ElementIter` | Lazy sync iterator over elements; each step releases the GIL |
| `AsyncElementIter` | Async iterator over elements; each step is a coroutine |
| `Span` | One styled text span: text, position, bbox, font identity, bold/italic/monospace/serif, underline/strikethrough, rise, vertical, invisible, color |
| `SpanIter` | Lazy sync iterator over a document's spans, buffering one page at a time |
| `AsyncSpanIter` | Async iterator over a document's spans |
| `PageImage` | One embedded image extracted from a page: native `width`/`height` and PNG-encoded `data` |
| `PdfError` | The exception type for any PDF processing error |

Guide chapters with runnable examples: [Extracting text](../guide/text.md), [Markdown output](../guide/markdown.md), [Styled spans](../guide/spans.md), [Rendering pages](../guide/rendering.md), [Extracting images](../guide/images.md), [Markdown to PDF](../guide/md-to-pdf.md), [Async and remote documents](../guide/async.md), [Encrypted documents](../guide/encryption.md).

## The md submodule

`pdfboss.md.to_pdf(markdown, theme=None, size="a4", landscape=False, base_dir=None)` composes CommonMark+GFM source into a themed PDF and returns the file bytes. `theme` is CSS source text, not a path; an unknown `size` raises `PdfError`; replaced characters and skipped raw HTML surface as one `UserWarning`. Details and examples in [Markdown to PDF](../guide/md-to-pdf.md).

That is Python's one creation path. Canvas-level creation — shapes, glyph runs, image placement — is the Rust crate [`pdfboss-write`](./rust.md) and the [`pdfboss create`](./cli.md#create) CLI.

## Error handling

Everything raises `PdfError`: bad or truncated data, unsupported encryption, stream decode failures and I/O errors, with the underlying detail in the message. Messages from the element and async APIs are prefixed by the layer they came from: `"parse: …"`, `"io: …"` or `"http: …"`.

```python
import pdfboss

try:
    doc = pdfboss.Document("report.pdf")
except pdfboss.PdfError as e:
    print(f"could not open: {e}")
```

Two conventional exceptions apply where Python conventions demand them: constructing a `Document` with neither or both of `path` and `data` raises `ValueError` (as does a non-positive `scale` or an unusable `fonts="full"` setup in `render`), and an out-of-range page index raises `IndexError`. Element iterators have salvage semantics: a per-item failure raises `PdfError` for that item, and iteration may be continued. Span iterators raise `PdfError` when a page cannot be materialized.

## Threading

A `Document` — and any `Page` it hands out — may be used from any thread. Access to the underlying parsed document is serialized internally, and `extract_text`/`render` release the GIL while they run, so other Python threads keep making progress during long extractions or renders. Element and span iteration release the GIL per step the same way. `Document.render_pages` fans page rendering out across the machine's cores.

The async API needs no thread juggling: `AsyncDocument`'s coroutines are driven by one global multi-thread tokio runtime, and `AsyncDocument.render_pages` fans out as tokio tasks so the asyncio loop stays free.
