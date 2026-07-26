<h1 align="center">pdfboss</h1>

<p align="center">
  <strong>A PDF engine written from scratch in Rust — parse, extract text, rasterize to PNG. One core, a CLI, and pythonic bindings.</strong>
</p>

<p align="center">
  <a href="https://github.com/4thel00z/pdfboss/actions/workflows/ci.yaml"><img src="https://github.com/4thel00z/pdfboss/actions/workflows/ci.yaml/badge.svg" alt="CI"></a>
  <a href="https://github.com/4thel00z/pdfboss/actions/workflows/python-ci.yml"><img src="https://github.com/4thel00z/pdfboss/actions/workflows/python-ci.yml/badge.svg" alt="python-ci"></a>
  <a href="https://pypi.org/project/pdfboss/"><img src="https://img.shields.io/pypi/v/pdfboss?logo=pypi&logoColor=white" alt="PyPI"></a>
  <img src="https://img.shields.io/badge/rust-2021-000000?logo=rust&logoColor=white" alt="Rust 2021">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="MIT OR Apache-2.0"></a>
</p>

---

## Motivation

Reading a PDF shouldn't mean linking a C library. pdfboss is a clean-room reader built straight from the ISO 32000 specification: no C dependencies, no bindings to anyone else's engine — just safe Rust with a small, obvious API. The same core powers a CLI and a native Python extension, so a script and a service share one implementation.

It is a **lenient reader**: real-world files are damaged, and pdfboss recovers rather than refuses — reconstructing broken cross-reference tables, tolerating wrong stream lengths, and skipping garbage operators instead of erroring out.

## Install

### Python

```bash
pip install pdfboss
```

Prebuilt abi3 wheels (CPython ≥ 3.12) for Linux and macOS; no toolchain required.

### Rust

```bash
cargo add pdfboss-core pdfboss-text pdfboss-render pdfboss-aio pdfboss-tui   # library crates
cargo install pdfboss-cli                                                    # the `pdfboss` binary
```

## Usage

### CLI

```bash
pdfboss info    report.pdf                 # version, page count, sizes, metadata
pdfboss text    report.pdf --page 2        # extract text (omit --page for all)
pdfboss render  report.pdf --page 1 -o page.png --scale 2.0
pdfboss obj     report.pdf 5               # pretty-print object 5
```

Explorer subcommands, each accepting a local path or an `http(s)://` URL (range-fetched, never downloaded whole):

```bash
pdfboss json    report.pdf                    # dump the document as a JSON value tree
pdfboss hex     report.pdf obj:5              # hexdump the file or a selected element
pdfboss q       report.pdf '.header.version'  # jq-style queries over the JSON tree
pdfboss tui     report.pdf                    # interactive terminal explorer
```

### Python

```python
import pdfboss

doc = pdfboss.Document("report.pdf")       # or Document(data=raw_bytes)
print(doc.page_count, doc.version, doc.metadata)

page = doc[0]
print(page.width, page.height, page.rotation)
text = page.extract_text()                 # or doc.extract_text() for all pages
png  = page.render(scale=2.0)              # PNG bytes

for element in doc.elements():             # lazy: physical + logical, byte spans included
    print(element.kind, element.span)

# Async access over files or http(s) URLs, without reading the whole document.
doc = await pdfboss.AsyncDocument.open_url("https://example.com/report.pdf")
async for element in doc.elements():
    print(element.kind, element.value)
```

### Rust

```rust
use pdfboss_core::Document;

let doc = Document::open("report.pdf")?;
let page = doc.page(0)?;

let text = pdfboss_text::extract_text(&doc, &page)?;
let pixmap = pdfboss_render::render_page(&doc, &page, 2.0)?;
pixmap.save_png("page.png")?;
```

## What's inside

| Crate | Responsibility |
|---|---|
| `pdfboss-core` | Tokenizer, object model, stream filters, cross-references, object streams, document & page tree, content-stream operators |
| `pdfboss-text` | Simple and CID/Type0 fonts, standard encodings, `ToUnicode` CMaps, positional text extraction |
| `pdfboss-render` | Anti-aliased vector rasterizer — paths, fills, strokes, clipping, color, images — to RGBA/PNG |
| `pdfboss-aio` | Async I/O: range-fetching document access over files or HTTP, without reading the whole file |
| `pdfboss-cli` | The `pdfboss` command-line tool |
| `pdfboss-tui` | Interactive terminal explorer (`pdfboss tui`), built on `pdfboss-aio` |
| `pdfboss-py` | PyO3 extension module (`pdfboss._pdfboss`) built with maturin |

**Supported:** classic, stream, and hybrid cross-references with recovery scanning · object streams · FlateDecode, LZWDecode, ASCII85Decode, ASCIIHexDecode, RunLengthDecode + PNG/TIFF predictors · DCTDecode (JPEG) images · Standard-handler decryption — RC4 and AES-128/256 (empty user password) · page-tree attribute inheritance · text extraction with `ToUnicode` and WinAnsi/MacRoman/Standard encodings · rasterization of paths, fills (nonzero & even-odd), strokes, transforms, clipping, image/form XObjects, and embedded-TrueType glyph outlines · lazy element iteration over physical (objects, xref sections, trailer, with byte spans) and logical (pages, fonts, images, annotations, content operators) elements.

## Benchmarks

Against other Python PDF libraries over 40 real-world PDFs (best-of-3 per file, aggregated over the files every library handled; pages/sec, higher is faster):

<p align="center">
  <img src="https://raw.githubusercontent.com/4thel00z/pdfboss/main/benchmarks/results.png" alt="pdfboss vs. Python PDF libraries" width="100%">
</p>

**pdfboss is the fastest library measured on both operations — including against the C-backed PyMuPDF.** On text extraction it reaches 1,539 pages/s versus PyMuPDF's 279 (≈5.5×), and 25–80× the pure-Python readers. On open + parse it reaches 19,114 pages/s versus PyMuPDF's 3,766 (≈5×): lazy page-tree loading means opening a document reads only its declared page count instead of parsing every page dictionary up front. Rendering is not compared — pdfboss's rasterizer does not yet paint every glyph, so timing it against full renderers would be misleading.

Numbers are machine-dependent; reproduce with [`benchmarks/bench.py`](benchmarks/README.md).

## Limitations

Rendered pages paint the outlines of **embedded TrueType** glyphs (Type0/`CIDFontType2` under Identity, and simple `/TrueType` fonts via their `cmap`). Text in other fonts (CFF/Type1 programs, the standard 14, subset fonts without a usable `cmap`) is still positioned but not drawn.

Not yet supported in v0.1 (they error or degrade gracefully, and are on the roadmap): password-protected documents (the empty user password is handled for both RC4 and AES) · non-TrueType glyph outlines (CFF/Type1) · shadings and tiling patterns · `JPXDecode` (JPEG 2000) · `JBIG2Decode` and `CCITTFaxDecode` images · soft masks and blend modes · annotation appearance streams.

Rendering is lenient: content pdfboss cannot read is skipped so the rest of the page still rasterizes. It says so rather than passing the result off as a faithful render — `pdfboss render` prints a warning line per dropped item on stderr and annotates its summary, the TUI preview raises a status-bar notice, and the libraries expose the detail through `render_page_reporting` (Rust) and `Page.render_reporting()` (Python), which return the pixels plus a report of everything dropped or approximated.

The sync and async APIs are not at parity on encryption: `Document`/`Page` (and the CLI's `info`/`text`/`render`/`obj`) decrypt empty-user-password RC4/AES files transparently, as above. `AsyncDocument` (`pdfboss tui`, any `http(s)://` target, and the Python `AsyncDocument`) currently rejects every encrypted document outright, real password or not — async decryption parity is a tracked follow-up.

## Development

```bash
cargo test --workspace          # Rust test suite
cargo clippy --workspace --all-targets -- -D warnings
maturin develop                 # build the Python extension into your venv
pytest                          # Python integration tests
```

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you shall be dual-licensed as above, without any additional terms or conditions.
