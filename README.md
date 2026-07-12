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
cargo add pdfboss-core pdfboss-text pdfboss-render   # library crates
cargo install pdfboss-cli                            # the `pdfboss` binary
```

## Usage

### CLI

```bash
pdfboss info    report.pdf                 # version, page count, sizes, metadata
pdfboss text    report.pdf --page 2        # extract text (omit --page for all)
pdfboss render  report.pdf --page 1 -o page.png --scale 2.0
pdfboss obj     report.pdf 5               # pretty-print object 5
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
| `pdfboss-cli` | The `pdfboss` command-line tool |
| `pdfboss-py` | PyO3 extension module (`pdfboss._pdfboss`) built with maturin |

**Supported:** classic, stream, and hybrid cross-references with recovery scanning · object streams · FlateDecode, LZWDecode, ASCII85Decode, ASCIIHexDecode, RunLengthDecode + PNG/TIFF predictors · DCTDecode (JPEG) images · page-tree attribute inheritance · text extraction with `ToUnicode` and WinAnsi/MacRoman/Standard encodings · rasterization of paths, fills (nonzero & even-odd), strokes, transforms, clipping, and image/form XObjects.

## Limitations

Not yet supported in v0.1 (they error or degrade gracefully, and are on the roadmap): encrypted documents · glyph painting in rendered pages (text is positioned, not drawn) · shadings and tiling patterns · `JPXDecode` (JPEG 2000).

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
