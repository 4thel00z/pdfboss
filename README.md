<h1 align="center">pdfboss</h1>

<p align="center">
  <strong>A PDF engine written from scratch in Rust: parse, extract text, rasterize to PNG. One core, a CLI, and pythonic bindings.</strong>
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

Reading a PDF should not require a C library. pdfboss is a clean-room reader built from the ISO 32000 specification. It has no C dependencies and no bindings to another engine. It is safe Rust with a small, obvious API. The same core powers the CLI and the native Python extension, so a script and a service share one implementation.

pdfboss is a **lenient reader**. Real-world files are damaged, so it recovers instead of refusing. It reconstructs broken cross-reference tables, tolerates wrong stream lengths, and skips garbage operators.

## Install

### Python

```bash
pip install pdfboss
```

Prebuilt abi3 wheels (CPython 3.12+) for Linux and macOS. No toolchain required.

### Rust

```bash
cargo add pdfboss-core pdfboss-text pdfboss-output pdfboss-render pdfboss-aio pdfboss-tui   # library crates
cargo install pdfboss-cli                                                                   # the `pdfboss` binary
```

## Usage

### CLI

```bash
pdfboss info    report.pdf                 # version, page count, sizes, metadata
pdfboss text    report.pdf --page 2        # extract text (omit --page for all)
pdfboss md      report.pdf                 # markdown: headings, lists, tables from layout
pdfboss render  report.pdf --page 1 -o page.png --scale 2.0
pdfboss obj     report.pdf 5               # pretty-print object 5
```

Explorer subcommands. Each accepts a local path or an `http(s)://` URL, fetched in ranges and never downloaded whole:

```bash
pdfboss json    report.pdf                    # dump the document as a JSON value tree
pdfboss json    report.pdf --layout           # ...plus per-page layout blocks
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
md   = doc.extract_markdown()              # headings, lists and tables inferred from layout
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

let text = pdfboss_output::extract_text(&doc, &page)?;
let markdown = pdfboss_output::extract_markdown(&doc)?;
let pixmap = pdfboss_render::render_page(&doc, &page, 2.0)?;
pixmap.save_png("page.png")?;
```

## What's inside

| Crate | Responsibility |
|---|---|
| `pdfboss-core` | Tokenizer, object model, stream filters, cross-references, object streams, document & page tree, content-stream operators |
| `pdfboss-text` | Simple and CID/Type0 fonts, standard encodings, `ToUnicode` CMaps, positional text spans |
| `pdfboss-output` | Layout analysis over those spans (lines, columns, headings, lists, tables, repeated page headers), rendered as plain text or Markdown |
| `pdfboss-jpx` | JPEG 2000 decoder for `JPXDecode` image streams, implemented from ITU-T T.800 |
| `pdfboss-icc` | ICC profile parser and colour transform to sRGB, implemented from ICC.1:2010 |
| `pdfboss-render` | Anti-aliased vector rasterizer (paths, fills, strokes, clipping, color, images, glyph outlines) to RGBA/PNG |
| `pdfboss-aio` | Async I/O: range-fetching document access over files or HTTP, without reading the whole file |
| `pdfboss-cli` | The `pdfboss` command-line tool |
| `pdfboss-tui` | Interactive terminal explorer (`pdfboss tui`), built on `pdfboss-aio` |
| `pdfboss-py` | PyO3 extension module (`pdfboss._pdfboss`) built with maturin |

**Supported:** classic, stream, and hybrid cross-references with recovery scanning · object streams · FlateDecode, LZWDecode, ASCII85Decode, ASCIIHexDecode, RunLengthDecode + PNG/TIFF predictors · DCTDecode (JPEG) images · JPXDecode (JPEG 2000) images: JP2 containers and raw codestreams, every progression order, both wavelets, palettes, and `/SMaskInData` alpha (ITU-T T.800) · CCITTFaxDecode scans: Group 3 one-dimensional, Group 3 mixed and Group 4 coding (ITU-T T.4/T.6) · JBIG2Decode scans: generic regions, symbol dictionaries and text regions, arithmetic- or Huffman-coded, MMR-coded generic regions and collective bitmaps, immediate generic refinement regions, with or without `/JBIG2Globals` · Standard-handler decryption: RC4 and AES-128/256, with the user or owner password (`--password`, `password=`; the empty user password opens transparently) · page-tree attribute inheritance · text extraction with `ToUnicode` and WinAnsi/MacRoman/Standard encodings · Markdown output with headings, lists, emphasis and pipe/HTML tables inferred from the page layout and from drawn table borders · rasterization of paths, fills (nonzero & even-odd), strokes, transforms, clipping, all blend modes (separable and non-separable), soft masks (image `/SMask`, stencil and color-key `/Mask`, and Luminosity/Alpha `/SMask` groups in `/ExtGState`), image/form XObjects, all seven shading types (the `sh` operator and shading-pattern fills and strokes: function-based, axial and radial through sampled, exponential, stitching and PostScript-calculator functions, plus Gouraud triangle meshes and Coons/tensor patch meshes), tiling patterns (colored and uncolored, each cell run as its own content stream), annotation normal appearances (`/AP` `/N`, with `/AS` state selection), `ICCBased` colour through the embedded profile (ICC.1:2010 v2/v4, matrix/TRC, grayTRC, and `A2B0` lookup transforms) and the CIE `CalRGB`/`CalGray`/`Lab` families through XYZ, and the glyph outlines of every embedded font program (TrueType, CFF, Type1, Type3), with optional substitution for non-embedded simple fonts · lazy element iteration over physical (objects, xref sections, trailer, with byte spans) and logical (pages, fonts, images, annotations, content operators) elements.

## Benchmarks

### Text and parsing

Against other Python PDF libraries over 40 real-world PDFs (best-of-3 per file, aggregated over the files every library handled; pages/sec, higher is faster):

<p align="center">
  <img src="https://raw.githubusercontent.com/4thel00z/pdfboss/main/benchmarks/results.png" alt="pdfboss vs. Python PDF libraries" width="100%">
</p>

**pdfboss is the fastest library measured on both operations, including against the C-backed PyMuPDF.** On text extraction it reaches 9,000 pages/s. PyMuPDF reaches 449 (about 20×), and the pure-Python readers are 95× to 500× slower. Since 0.9.0, `doc.extract_text()` spreads pages across cores, which widened the gap over the sequential libraries from the 7× measured before that landed. On open + parse it reaches 357,000 pages/s against PyMuPDF's 99,000 (about 3.6×). Lazy page-tree loading means opening a document reads only its declared page count instead of parsing every page dictionary up front. Opening is close to free, so the ratio says more about what the others do eagerly than about pdfboss. Rendering is compared in its own section below, restricted to the files pdfboss provably rasterizes completely — timing it against full renderers on the rest would credit it for work it skips.

Numbers are machine-dependent; reproduce with [`benchmarks/bench.py`](benchmarks/README.md).

### Extraction quality

Speed without fidelity is worthless, so extraction quality is measured too. The corpus is [opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench) (200 real-world PDFs), which PDF-to-Markdown engines use for their published comparisons. pdfboss appears twice: the default plain-text output and the Markdown adapter behind `pdfboss md`. The metric that is comparable across all rows is **NID**, the reading-order similarity against the ground truth (0 to 1, higher is better).

| Engine | Reading order (NID) | Output | Time (200 docs) |
|---|--:|---|--:|
| pdf-inspector 0.2.6 | 0.915 | Markdown | 0.44s |
| liteparse 2.10.1 | 0.913 | Markdown | 0.75s |
| opendataloader 2.2.1 | 0.902 | Markdown | 2.57s |
| pymupdf4llm 0.2.0 | 0.886 | Markdown | 17.12s |
| **pdfboss** (`md`) | **0.877** | Markdown | **0.14s** |
| **pdfboss** | **0.868** | plain text | **0.13s** |
| markitdown 0.1.5 | 0.844 | Markdown | 16.17s |

pdfboss reads the whole corpus in about a seventh of a second in either mode. That is over 3× faster than the fastest competing Markdown engine measured on the same machine. Its reading-order score sits in the middle of the field: per document, the plain-text output beats pdf-inspector's NID on 105 of the 200 files, ties on 23 and loses on 72. The losses concentrate in table regions, where structured output matches the ground truth more closely than flowed text can. On the benchmark's combined metric the Markdown adapter scores 0.801 (reading order 0.877, headings and lists 0.667, table structure 0.532). It detects tables from column gaps and from drawn borders, so bordered grids and boxed lists without column gaps are found too. Two-column layouts are read column-major. Justified text keeps its word spacing. Ligatures and small-caps variants decode through the full Adobe Glyph List conventions.

Quality rows come from the benchmark's own evaluator over all 200 documents. The two pdfboss timings were measured together in one session on an Apple M3 Pro under the benchmark's protocol: median of five single-process runs after a warm-up, wheel built from main. pdf-inspector was measured the same way on the same machine in an earlier session. The other engines' timings are the ones [published with the corpus](https://github.com/firecrawl/opendataloader-bench/tree/abi/pdf-parser-benchmark-results) from an Apple M4 Pro. Read them as order-of-magnitude context, not a same-machine race.

### Rendering

A renderer that skips work looks fast, and pdfboss does not paint everything yet. So the render benchmark certifies every file before the stopwatch starts: pdfboss rasterizes each page through `render_reporting` at the `full` fonts tier — substituting non-embedded fonts, which is what the other engines do by default — and a file where any page reports dropped or approximated content is excluded, with its reason printed and counted. A second gate renders each file's first page in every library and excludes files whose ink coverage disagrees, which catches work skipped without a report: a blank page renders instantly and means nothing. Of the 40-file sample above, 37 files (864 pages) certify. The three annotation-appearance files and the tiling-pattern file the earlier sample excluded now certify, leaving only three files whose fonts lack a glyph for a code the page draws.

| Library | pages/sec |
|---|--:|
| pypdfium2 | 125.9 |
| pdfboss | 112.0 |
| pdfplumber (via pdfium) | 104.5 |
| PyMuPDF | 94.8 |

pdfboss rasterizes the mixed corpus second only to pdfium itself: about 11% behind it, ahead of pdfplumber's pdfium stack and PyMuPDF, with no C in it. Caching loaded shadings and patterns across paints and reworking the rasterizer's hot paths moved it up a place since the last table, with every output byte identical. Compare the rows against each other, not against another machine's numbers. Across three back-to-back passes every library repeated within 1.6%, the pdfboss-to-pdfium ratio held within 0.5%, and the ordering never moved.

Reproduce with [`benchmarks/bench_render.py`](benchmarks/README.md).

### Scanned documents

Scans are the other half of the world's PDFs, and they are a different workload: one full-page bilevel image per page, JBIG2- or CCITT-coded, with no text operators at all. Rendering **is** comparable there, because with no glyphs to paint every library draws the same picture. So scans get their own benchmark: a 544-page JBIG2 book (1994 × 2832 samples per page) rasterized to PNG at 1:1.

| Library | pages/sec | Ink on page 1 |
|---|--:|--:|
| pdfboss | 88.1 | 4.83% |
| pdfplumber (via pdfium) | 57.8 | 4.87% |
| PyMuPDF | 54.5 | 4.82% |
| pypdfium2 | 52.6 | 4.85% |

**pdfboss is the fastest of the four here, at about 1.5× the C-backed renderers**, and the only one of them with no C in it. Compare the four rows against each other, not against another machine's numbers. All four are timed in one pass, and the ratio has landed within a few percent of 1.5× on every run (1.49× to 1.56×), while the absolute numbers varied by half as the machine warmed and cooled.

What is left is the codec itself. Four fifths of the time goes to the JBIG2 arithmetic decoder and the context formation that feeds it. That part is a serial dependency chain: every decision needs the interval state the previous one wrote, and every pixel's context contains the pixels just decoded. It neither vectorizes nor parallelizes. The rest was arithmetic that did not need doing: expanding a packed scan into eight times its size in RGBA before sampling a fraction of it, blending opaque pixels through an alpha formula that returns them unchanged, and walking bitmaps a pixel at a time where a row of bytes would do.

The ink column is what makes the timings mean anything. A library that cannot decode a scan's codec usually hands back a blank page instead of raising, and a blank page benchmarks superbly. Agreeing coverage says all four decoded the same picture. They do not agree pixel for pixel, because each library downsamples 1994 × 2832 samples onto a 462 × 663 page with its own resampling.

Reproduce with [`benchmarks/bench_scans.py`](benchmarks/README.md).

### In the browser

[pdfarena](https://pdfarena.tahrioui.de) races pdfboss against hayro, pdf.js and PDFium on any PDF you drop in, with pdfboss and hayro compiled to WebAssembly. Each engine renders in its own web worker, the stopwatch wraps only the render call, and every challenger is pixel-diffed against pdf.js as the reference. Nothing gets uploaded; the whole benchmark runs in your browser.

## Limitations

Glyph painting is staged in tiers, selected with `--fonts`. The default, `all-embedded`, paints every embedded font program (TrueType, CFF, Type1 and Type3). `embedded-only` restricts that to TrueType. `full` additionally substitutes a replacement face for a **non-embedded** simple font, from either a directory you supply or the compiled-in OFL Croscore set (behind the `substitute-fonts` feature). Standard-14 advance widths come from the Adobe Core-14 AFM tables when a substitute is used, behind the PDF's own `/Widths`.

What still does not paint: `/Symbol` and `/ZapfDingbats` have no license-clean substitute, so they stay blank at every tier rather than borrowing an unrelated face's glyphs. A bold *sans* substitute is not visually distinct from regular weight. Text a tier leaves unpainted still advances — through the PDF's own `/Widths`, or the Adobe Core-14 AFM tables for a standard-14 face — so everything painted around it stays where the page put it.

`JBIG2Decode` covers generic regions (all four templates, with TPGDON, arithmetic or MMR-coded), symbol dictionaries and text regions in both the arithmetic and the Huffman variant, immediate generic refinement regions (both templates, with TPGRON), and custom code table segments. That is what scanners actually emit, but it is not the whole standard, and the rest is refused rather than approximated. A stream that uses refinement inside a symbol dictionary or a text region, an intermediate region of any kind, or pattern dictionaries and halftone regions fails with a message that names the feature. A scan that will not decode says why on the first try.

Colour converts to sRGB. `ICCBased` spaces parse their embedded profile (v2 and v4; matrix/TRC and grayTRC models, and `A2B0` lookup pipelines): a profile equivalent to sRGB keeps the exact device-RGB path, others transform per colour with Bradford adaptation from the D50 connection space, and a profile that will not parse falls back to the `/N` channel-count reduction. `CalRGB`, `CalGray`, and `Lab` convert through CIE XYZ the same way. Only a profile's default transform is used — rendering intents are not switched — and `DeviceN` keeps a tint approximation.

`JPXDecode` implements ITU-T T.800 (JPEG 2000 Part 1) with known approximations, each reported as a render warning rather than passed off silently. ICC profiles embedded in the JPEG 2000 container itself are not interpreted; that colour is approximated from the channel count, and sYCC conversion is approximate. Part 2 (ISO/IEC 15444-2) extensions are tolerated in the container but not decoded. Every output sample is normalized to 8 bits per channel, so sources deeper than 8 bits (the spec allows up to 38) lose their extra precision.

Optional content groups (PDF layers, ISO 32000 §8.11) are honored per the document's default configuration: rendering and text extraction skip layers it turns off, counting them on the reports' `hidden` counters.

Not yet supported (they error or degrade gracefully, and are on the roadmap): the JBIG2 features listed above · the unpainted faces listed above.

Rendering is lenient: content pdfboss cannot read is skipped so the rest of the page still rasterizes. It says so rather than passing the result off as a faithful render. `pdfboss render` prints a warning line per dropped item on stderr and annotates its summary. The TUI preview raises a status-bar notice. The libraries expose the detail through `render_page_reporting` (Rust) and `Page.render_reporting()` (Python), which return the pixels plus a report of everything dropped or approximated.

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
