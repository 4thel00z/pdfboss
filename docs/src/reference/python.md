# Python API reference

The `pdfboss` package re-exports the compiled extension module `pdfboss._pdfboss`. Its public surface is twelve top-level classes, the `md` and `write` submodules and the `__version__` string; the typed stubs in [`_pdfboss.pyi`](https://github.com/4thel00z/pdfboss/blob/main/python/pdfboss/_pdfboss.pyi) are the authoritative reference for every signature and docstring. This chapter is the inventory; worked examples live in the guide chapters.

## The twelve classes

| Name | What it is |
|---|---|
| `Document` | A loaded PDF, from a path or bytes; pages by index, the `metadata` property, `extract_text`, `extract_markdown`, `render_pages`, `elements`, `spans` |
| `Page` | One page: geometry (width/height/rotation and the five boxes), `extract_text`, `extract_markdown`, `spans`, `render`, `render_reporting`, `extract_images` |
| `ReadingOrder` | `CONTENT`, `STRUCTURE_TREE`, `GEOMETRIC`: the `reading_order` keyword every extraction method takes, as the enum or its string value |
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

`Document.metadata` is a property returning the document information
dictionary as a `dict[str, str]`, only keys present in the file included;
`AsyncDocument.metadata()` is a coroutine yielding the same mapping.
`AsyncDocument` also has `page(index)` (synchronous, 0-based, no negative
indexes; subscription `doc[i]` accepts them) and `get_object(num, gen=0)`, a
coroutine fetching one indirect object and returning it through the same
plain-Python conversion as `Element.value()`.

Guide chapters with runnable examples: [Extracting text](../guide/text.md), [Markdown output](../guide/markdown.md), [Styled spans](../guide/spans.md), [Rendering pages](../guide/rendering.md), [Extracting images](../guide/images.md), [Creating PDFs](../guide/creating.md), [Markdown to PDF](../guide/md-to-pdf.md), [Async and remote documents](../guide/async.md), [Encrypted documents](../guide/encryption.md), [Editing PDFs](../guide/editing.md), [Assembling documents](../guide/assembling.md).

## The md submodule

`pdfboss.md.to_pdf(markdown, theme=None, size="a4", landscape=False, base_dir=None)` composes CommonMark+GFM source into a themed PDF and returns the file bytes. `theme` is CSS source text, not a path; an unknown `size` raises `PdfError`; replaced characters and skipped raw HTML surface as one `UserWarning`. Details and examples in [Markdown to PDF](../guide/md-to-pdf.md).

Canvas-level and element-level creation from Python is the `write` submodule below.

## The write submodule

`pdfboss.write` composes new PDFs from frozen values joined with `|`; the same vocabulary is the Rust crate [`pdfboss-write`](./rust.md) and the [`pdfboss create`](./cli.md#create) CLI, and worked examples live in [Creating PDFs](../guide/creating.md#python-pdfbosswrite). Fourteen classes:

| Name | What it is |
|---|---|
| `Pdf` | A document under construction; composes pages, attachments and page labels (appended) and the singleton `Metadata`, `Outline` and `Viewer` slots; `save(path)` and `to_bytes()` serialize |
| `Page` | One page (`size` names a size case-insensitively, default `"a4"`; `landscape` swaps width and height); composes `Text`, `Image`, `Link`, `Paragraph` or any draw object |
| `Text` | One line of text: `value`, `at=(x, y)` baseline origin, `font` (default `Standard14.HELVETICA`), `size` (default 12.0), optional `(r, g, b)` `color` |
| `Paragraph` | Wrapped, aligned text: `text`, `rect`, `font`, `size` (default 11.0), `leading` (default derived from `size`), `align` of `left`, `center`, `right` or `justify` |
| `Image` | A placed raster: `data` as a path string or bytes, `at`, optional `width`/`height`; either source is read and decoded at `save`/`to_bytes` time |
| `Link` | A clickable rectangle: `rect` plus exactly one of `url` or `page` (0-based) |
| `Bookmark` | One outline entry: `title`, `page`, keyword-only `children`; nests by construction |
| `Outline` | The bookmark panel, `Outline(*bookmarks)`; a singleton `Pdf` slot |
| `Attachment` | An embedded file: `name`, `data`, optional `mime` and `description`; carries no dates |
| `PageLabel` | One page-numbering range: `first_page` (0-based), optional `style` (`decimal`, `roman-upper`, `roman-lower`, `letters-upper`, `letters-lower`), `prefix`, `start_at` (default 1) |
| `Viewer` | Opening preferences: `layout`, `mode`, `open_to`; a singleton `Pdf` slot |
| `Metadata` | `/Info` text fields: `title`, `author`, `subject`, `keywords`, `creator`, `producer`; a singleton `Pdf` slot |
| `Standard14` | The fourteen standard faces as SCREAMING_SNAKE members: `HELVETICA`, `TIMES_BOLD_ITALIC`, `ZAPF_DINGBATS`, … |
| `Canvas` | The painting surface handed to a draw object's `draw` method; it has no public constructor |

Every `|` returns a new value and leaves the receiver unchanged; copies are cheap handle clones, and nothing is built until `save` or `to_bytes`, which lower the composition once under the GIL and release it to serialize. `to_bytes` may be called repeatedly, since the composed value is never consumed.

The draw protocol is structural: any object with a callable `draw` attribute composes onto a `Page`, and its `draw(canvas)` receives a `Canvas` with twelve methods (`text`, `line`, `rect`, `move_to`, `line_to`, `curve_to`, `close`, `stroke`, `fill`, `set_fill`, `set_stroke`, `set_line_width`) that paints in content order. The stub declares a `Draw` protocol type for checkers only; there is no runtime `Draw` class to import or inherit. The canvas is only usable inside the call, and every method raises `PdfError` once `draw` has returned.

Two more names work on existing files, neither in the table above since neither composes with `|`: `watermark(data, overlay, *, rewrite=False, under=False)` takes two PDFs as bytes and returns `data` with the first page of `overlay` drawn over every page, as an incremental update appended to `data`'s bytes, or with `rewrite=True` as a fresh, compressed file; `under=True` draws the overlay beneath each page's own content instead of over it (see [Watermarking an existing file](../guide/creating.md#watermarking-an-existing-file)); `Update(doc)` stages a metadata edit over an existing `Document`, with `set_metadata(title=None, author=None, subject=None, keywords=None, creator=None, producer=None)` merging the given fields and `save(path)` or `to_bytes()` writing the result as an incremental update (see [Editing PDFs](../guide/editing.md)). Both release the GIL while they run. `Update` always raises `PdfError` for an encrypted base, whether or not it opens. `watermark`'s default (`rewrite=False`, appending) does the same for an encrypted `data` or `overlay`; with `rewrite=True`, only a `data` or `overlay` that cannot be opened at all raises `PdfError`, and one that opens under its password, including the empty user password, copies across as plain, unencrypted bytes instead.

Four more functions assemble documents from existing file bytes, also outside the table since none composes with `|` (see [Assembling documents](../guide/assembling.md)): `merge(inputs)` gathers each item, raw `bytes` for every page or a `(bytes, list[int])` tuple for specific 0-based pages, into one fresh document in argument order; `split(data, every)` cuts `data` into consecutive parts of `every` pages, the last part carrying whatever remains; `rotate(data, by, pages=None, rewrite=False)` turns `pages` (0-based, every page when omitted) by `by` degrees clockwise, restricted to 90, 180 or 270 (else `ValueError`), appending an incremental update by default or writing a fresh file with `rewrite=True`; `rewrite(data)` writes `data` fresh, recompressed with unreachable objects and earlier update sections left behind. `merge`, `split` and `rewrite` always produce a fresh file; only `rotate` can append instead, the same choice `Update`/`meta` offers. An input that cannot be opened at all (encrypted with no working password) raises `PdfError` immediately, before any assembly work begins. One that opens under its password, including the empty user password, passes through `merge`, `split` and `rewrite` as plain, unencrypted bytes, and through `rotate` the same way when `rewrite=True`; `rotate`'s default appending mode still raises `PdfError` for any encrypted input, opened or not. An owner-password-only file therefore loses its `/P` restrictions the moment its holder runs it through `rewrite` (or `merge`, `split`, or `rotate(rewrite=True)`): the freshly written output carries no encryption at all, restrictions included. `rotate` also refuses, in either mode, a page inlined directly into `/Kids` with no object of its own, since pdfboss does not yet restructure such a page to rotate it.

Two more functions protect or remove protection from a document's bytes, also outside the table above (see [Encrypted PDFs](../guide/encryption.md)): `encrypt(data, *, user_password="", owner_password="", allow=None)` AES-256, revision-6 protects `data` and returns the fresh file, restricted by `allow`, a list drawn from `print`, `modify`, `copy`, `annotate`, `fill-forms`, `accessibility`, `assemble` and `print-hires` (every permission when omitted); an unknown `allow` value raises `ValueError` naming it, and so does leaving both `user_password` and `owner_password` empty, while an empty `owner_password` alone falls back to `user_password`. `decrypt(data, *, password="")` opens `data` under its user or owner password and returns a fresh, unencrypted file; a wrong or missing password raises `PdfError`. Both release the GIL around the actual work.

## Error handling

Everything raises `PdfError`: bad or truncated data, unsupported encryption, stream decode failures and I/O errors, with the underlying detail in the message. Messages from the element and async APIs are prefixed by the layer they came from: `"parse: …"`, `"io: …"` or `"http: …"`.

```python
import pdfboss

try:
    doc = pdfboss.Document("report.pdf")
except pdfboss.PdfError as e:
    print(f"could not open: {e}")
```

Two conventional exceptions apply where Python conventions demand them: constructing a `Document` with neither or both of `path` and `data` raises `ValueError` (as does a non-positive `scale`, an unknown `fonts=`, `compression=` or `format=` string, a `quality=` outside 1 to 100, or an unusable `fonts="full"` setup in `render`), and an out-of-range page index raises `IndexError`. Element iterators have salvage semantics: a per-item failure raises `PdfError` for that item, and iteration may be continued. Span iterators raise `PdfError` when a page cannot be materialized.

The `write` submodule splits its failures by phase. `TypeError` is raised at construction for a `Link` with neither or both of `url` and `page`, an unknown `align`, page-label `style`, viewer `layout` or `mode`, or `Image` data that is neither `str` nor `bytes`; and at composition for an unsupported `|` operand or a second `Metadata`, `Outline` or `Viewer`. Everything that fails while lowering (an unreadable or undecodable image file, a paragraph overflowing its rect, an unencodable character, a target page out of range) raises `PdfError` from `save`/`to_bytes`. A draw object's `Canvas` stops working the moment its `draw` call returns: any later method call on it raises `PdfError` at that call site. An exception raised inside `draw()` itself propagates from `save`/`to_bytes` exactly as the Python code raised it.

## Threading

A `Document` (and any `Page` it hands out) may be used from any thread. Access to the underlying parsed document is serialized internally, and `extract_text`/`render` release the GIL while they run, so other Python threads keep making progress during long extractions or renders. Element and span iteration release the GIL per step the same way. `Document.render_pages` fans page rendering out across the machine's cores.

The async API needs no thread juggling: `AsyncDocument`'s coroutines are driven by one global multi-thread tokio runtime, and `AsyncDocument.render_pages` fans out as tokio tasks so the asyncio loop stays free.
