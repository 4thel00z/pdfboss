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
| `pdfboss-jpx` | JPEG 2000 decoder for `JPXDecode` image streams, implemented from ITU-T T.800 |
| `pdfboss-render` | Anti-aliased vector rasterizer — paths, fills, strokes, clipping, color, images, glyph outlines — to RGBA/PNG |
| `pdfboss-aio` | Async I/O: range-fetching document access over files or HTTP, without reading the whole file |
| `pdfboss-cli` | The `pdfboss` command-line tool |
| `pdfboss-tui` | Interactive terminal explorer (`pdfboss tui`), built on `pdfboss-aio` |
| `pdfboss-py` | PyO3 extension module (`pdfboss._pdfboss`) built with maturin |

**Supported:** classic, stream, and hybrid cross-references with recovery scanning · object streams · FlateDecode, LZWDecode, ASCII85Decode, ASCIIHexDecode, RunLengthDecode + PNG/TIFF predictors · DCTDecode (JPEG) images · JPXDecode (JPEG 2000) images — JP2 containers and raw codestreams, every progression order, both wavelets, palettes, and `/SMaskInData` alpha (ITU-T T.800) · CCITTFaxDecode scans — Group 3 one-dimensional, Group 3 mixed and Group 4 coding (ITU-T T.4/T.6) · JBIG2Decode scans — generic regions, symbol dictionaries and text regions, arithmetic- or Huffman-coded, MMR-coded generic regions and collective bitmaps, immediate generic refinement regions, with or without `/JBIG2Globals` · Standard-handler decryption — RC4 and AES-128/256 (empty user password) · page-tree attribute inheritance · text extraction with `ToUnicode` and WinAnsi/MacRoman/Standard encodings · rasterization of paths, fills (nonzero & even-odd), strokes, transforms, clipping, image/form XObjects, and the glyph outlines of every embedded font program (TrueType, CFF, Type1, Type3), with optional substitution for non-embedded simple fonts · lazy element iteration over physical (objects, xref sections, trailer, with byte spans) and logical (pages, fonts, images, annotations, content operators) elements.

## Benchmarks

### Text and parsing

Against other Python PDF libraries over 40 real-world PDFs (best-of-3 per file, aggregated over the files every library handled; pages/sec, higher is faster):

<p align="center">
  <img src="https://raw.githubusercontent.com/4thel00z/pdfboss/main/benchmarks/results.png" alt="pdfboss vs. Python PDF libraries" width="100%">
</p>

**pdfboss is the fastest library measured on both operations — including against the C-backed PyMuPDF.** On text extraction it reaches 3,185 pages/s versus PyMuPDF's 458 (≈7×), and 32–170× the pure-Python readers. On open + parse it reaches 361,000 pages/s versus PyMuPDF's 98,000 (≈4×): lazy page-tree loading means opening a document reads only its declared page count instead of parsing every page dictionary up front, so opening is close to free and the ratio says more about what the others do eagerly than about pdfboss. Rendering is not compared on this corpus — a few faces still go unpainted (see Limitations), so timing it against full renderers would flatter pdfboss for work it skipped. The scanned-document benchmark below is the render comparison, and it is fair precisely because a scan has no glyphs in it.

Numbers are machine-dependent; reproduce with [`benchmarks/bench.py`](benchmarks/README.md).

### Scanned documents

Scans are the other half of the world's PDFs, and they are a different workload: one full-page bilevel image per page, JBIG2- or CCITT-coded, with no text operators at all. Rendering **is** comparable there — with no glyphs to paint, every library draws the same picture — so it gets its own benchmark, over a 544-page JBIG2 book (1994 × 2832 samples per page) rasterized to PNG at 1:1.

| Library | pages/sec | Ink on page 1 |
|---|--:|--:|
| pdfboss | 88.5 | 4.83% |
| pdfplumber (via pdfium) | 59.5 | 4.87% |
| pypdfium2 | 55.6 | 4.85% |
| PyMuPDF | 55.3 | 4.82% |

**pdfboss is the fastest of the four here, at about 1.5× the C-backed renderers** — and the only one of them with no C in it. Compare the four rows against each other rather than against another machine's: all four are timed in one pass, and that ratio has landed within a few percent of 1.5× on every run (1.49× to 1.56×), across absolute numbers that varied by half as the machine warmed and cooled.

What is left is the codec itself. Four fifths of the time goes to the JBIG2 arithmetic decoder and the context formation feeding it, and that part is a serial dependency chain — every decision needs the interval state the previous one wrote, and every pixel's context contains the pixels just decoded — so it neither vectorizes nor parallelizes. The rest was arithmetic that did not need doing: expanding a packed scan into eight times its size in RGBA before sampling a fraction of it, blending opaque pixels through an alpha formula that returns them unchanged, and walking bitmaps a pixel at a time where a row of bytes would do.

The ink column is what makes the timings mean anything: a library that cannot decode a scan's codec usually hands back a blank page instead of raising, and a blank page benchmarks superbly. Agreeing coverage says all four decoded the same picture. They do not agree pixel for pixel — each downsamples 1994 × 2832 samples onto a 462 × 663 page with its own resampling.

Reproduce with [`benchmarks/bench_scans.py`](benchmarks/README.md).

## Limitations

Glyph painting is staged in tiers, selected with `--fonts`. The default, `all-embedded`, paints every embedded font program — TrueType, CFF, Type1 and Type3. `embedded-only` restricts that to TrueType, and `full` additionally substitutes a replacement face for a **non-embedded** simple font, from either a directory you supply or the compiled-in OFL Croscore set (behind the `substitute-fonts` feature). Standard-14 advance widths come from the Adobe Core-14 AFM tables when a substitute is used, behind the PDF's own `/Widths`.

What still does not paint: `/Symbol` and `/ZapfDingbats` have no license-clean substitute, so they stay blank at every tier rather than borrowing an unrelated face's glyphs. A bold *sans* substitute is not visually distinct from regular weight. And non-embedded text left unpainted at `all-embedded` is not yet advanced through the AFM tables, so its positioning drifts.

`JBIG2Decode` covers generic regions (all four templates, with TPGDON, arithmetic or MMR-coded), symbol dictionaries and text regions in both the arithmetic and the Huffman variant, immediate generic refinement regions (both templates, with TPGRON), and custom code table segments. That is what scanners actually emit, but it is not the whole standard, and the rest is refused rather than approximated — a stream using refinement inside a symbol dictionary or a text region, an intermediate region of any kind, pattern dictionaries or halftone regions fails with a message naming the feature, so a scan that will not decode says why on the first try.

Not yet supported (they error or degrade gracefully, and are on the roadmap): password-protected documents (the empty user password is handled for both RC4 and AES) · shadings and tiling patterns · the JBIG2 features listed above · the unpainted faces listed above · soft masks and blend modes · annotation appearance streams.

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
