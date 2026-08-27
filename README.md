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

Reading a PDF should not require a C library. pdfboss is a clean-room reader built from the ISO 32000 specification: safe Rust, no C dependencies, no bindings to another engine, one core behind the CLI and the native Python extension. It is a **lenient reader** — real-world files are damaged, so it reconstructs broken cross-reference tables, tolerates wrong stream lengths, and skips garbage operators instead of refusing.

## Install

```bash
pip install pdfboss           # prebuilt abi3 wheels (CPython 3.12+), no toolchain required
cargo install pdfboss-cli     # the `pdfboss` binary
```

## Usage

```bash
pdfboss info    report.pdf                 # version, page count, sizes, metadata
pdfboss text    report.pdf --page 2        # extract text (omit --page for all)
pdfboss md      report.pdf                 # markdown: headings, lists, tables from layout
pdfboss render  report.pdf --page 1 -o page.png --scale 2.0
pdfboss images  report.pdf -o out/         # extract embedded images as native-size PNGs
pdfboss tui     report.pdf                 # interactive terminal explorer
pdfboss create blank  -o out.pdf --pages 3    # new PDF: empty pages
pdfboss create text   notes.txt -o out.pdf    # new PDF: word-wrapped text
pdfboss create images a.png b.jpg -o out.pdf  # new PDF: one page per image
```

```python
import pdfboss

doc = pdfboss.Document("report.pdf")       # or Document(data=raw_bytes)
text = doc.extract_text()
md   = doc.extract_markdown()              # headings, lists and tables inferred from layout
png  = doc[0].render(scale=2.0)            # PNG bytes
imgs = doc[0].extract_images()             # embedded images: .data (PNG), .width, .height
```

<details>
<summary><strong>More: explorer subcommands, async Python, Rust</strong></summary>

Explorer subcommands. Each accepts a local path or an `http(s)://` URL, fetched in ranges and never downloaded whole:

```bash
pdfboss json    report.pdf                    # dump the document as a JSON value tree
pdfboss json    report.pdf --layout           # ...plus per-page layout blocks
pdfboss hex     report.pdf obj:5              # hexdump the file or a selected element
pdfboss q       report.pdf '.header.version'  # jq-style queries over the JSON tree
pdfboss obj     report.pdf 5                  # pretty-print object 5
```

```python
page = doc[0]
print(page.width, page.height, page.rotation)

for element in doc.elements():             # lazy: physical + logical, byte spans included
    print(element.kind, element.span)

# Async access over files or http(s) URLs, without reading the whole document.
doc = await pdfboss.AsyncDocument.open_url("https://example.com/report.pdf")
async for element in doc.elements():
    print(element.kind, element.value)
```

Rust — the library crates are on crates.io (`cargo add pdfboss-core pdfboss-text pdfboss-output pdfboss-render pdfboss-aio pdfboss-tui`):

```rust
use pdfboss_core::Document;

let doc = Document::open("report.pdf")?;
let page = doc.page(0)?;

let text = pdfboss_output::extract_text(&doc, &page)?;
let markdown = pdfboss_output::extract_markdown(&doc)?;
let pixmap = pdfboss_render::render_page(&doc, &page, 2.0)?;
pixmap.save_png("page.png")?;
let images = pdfboss_render::extract_page_images(&doc, &page)?; // native-size RGBA pixmaps
```

</details>

## Benchmarks

### Text and parsing

Against other Python PDF libraries over 40 real-world PDFs (pages/sec, higher is faster):

<p align="center">
  <img src="https://raw.githubusercontent.com/4thel00z/pdfboss/main/benchmarks/results.png" alt="pdfboss vs. Python PDF libraries" width="100%">
</p>

**pdfboss is the fastest library measured on both operations, including against the C-backed PyMuPDF and the Rust-backed pdf_oxide**: 6,700 pages/s extracting text against PyMuPDF's 460 (about 15×) and pdf_oxide's 300 (about 22×), and 383,000 pages/s opening + parsing against pdf_oxide's 173,000 (about 2.2×).

<details>
<summary><strong>Method and fine print</strong></summary>

Best-of-3 per file, aggregated over the files every library handled; measured with pdfboss 0.20.0 on an Apple M3 Pro, every table on this page from one session. The pure-Python readers are roughly 70× to 360× slower on extraction. Since 0.9.0, `doc.extract_text()` spreads pages across cores, which widened the gap over the sequential libraries from the 7× measured before that landed; since 0.19.0 every span also carries its style (font, weight, decorations, color), which costs the extraction rows a few percent against older tables. Lazy page-tree loading means opening a document reads only its declared page count instead of parsing every page dictionary up front. Opening is close to free, so the ratio says more about what the others do eagerly than about pdfboss. Rendering is compared in its own section below, restricted to the files pdfboss provably rasterizes completely — timing it against full renderers on the rest would credit it for work it skips.

Numbers are machine-dependent; reproduce with [`benchmarks/bench.py`](benchmarks/README.md).

</details>

### Extraction quality

On [opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench) — the 200-PDF corpus PDF-to-Markdown engines use for their published comparisons — pdfboss reads the whole corpus in about a seventh of a second, about 3× faster than the fastest competing Markdown engine, with a mid-field reading-order score (**NID**, higher is better):

| Engine | Reading order (NID) | Output | Time (200 docs) |
|---|--:|---|--:|
| pdf-inspector 0.2.6 | 0.915 | Markdown | 0.44s |
| liteparse 2.10.1 | 0.913 | Markdown | 0.75s |
| opendataloader 2.2.1 | 0.902 | Markdown | 2.57s |
| pymupdf4llm 0.2.0 | 0.886 | Markdown | 17.12s |
| **pdfboss** (`md`) | **0.877** | Markdown | **0.15s** |
| **pdfboss** | **0.868** | plain text | **0.16s** |
| markitdown 0.1.5 | 0.844 | Markdown | 16.17s |

<details>
<summary><strong>What the score is made of, and how it was measured</strong></summary>

Per document, the plain-text output beats pdf-inspector's NID on 105 of the 200 files, ties on 23 and loses on 72. The losses concentrate in table regions, where structured output matches the ground truth more closely than flowed text can. On the benchmark's combined metric the Markdown adapter scores 0.801 (reading order 0.877, headings and lists 0.667, table structure 0.532). It detects tables from column gaps and from drawn borders, so bordered grids and boxed lists without column gaps are found too. Two-column layouts are read column-major. Justified text keeps its word spacing. Ligatures and small-caps variants decode through the full Adobe Glyph List conventions.

Quality rows come from the benchmark's own evaluator over all 200 documents. The two pdfboss timings were measured together in one session on an Apple M3 Pro under the benchmark's protocol: median of five single-process runs after a warm-up, wheel built from main. pdf-inspector was measured the same way on the same machine in an earlier session. The other engines' timings are the ones [published with the corpus](https://github.com/firecrawl/opendataloader-bench/tree/abi/pdf-parser-benchmark-results) from an Apple M4 Pro. Read them as order-of-magnitude context, not a same-machine race.

</details>

### Rendering

A renderer that skips work looks fast, so every file is certified before the stopwatch starts: any page that reports dropped or approximated content excludes its file, and an ink-coverage gate across libraries catches work skipped silently. 38 of the 40 files (888 pages) certify:

| Library | pages/sec |
|---|--:|
| pypdfium2 | 122.0 |
| pdfboss | 112.7 |
| pdfplumber (via pdfium) | 103.9 |
| PyMuPDF | 91.9 |

pdfboss rasterizes the mixed corpus second only to pdfium itself — about 8% behind it, ahead of pdfplumber's pdfium stack and PyMuPDF — with no C in it.

<details>
<summary><strong>Certification and stability details</strong></summary>

pdfboss rasterizes each page through `render_reporting` at the `full` fonts tier — substituting non-embedded fonts, which is what the other engines do by default — and a file where any page reports dropped or approximated content is excluded, with its reason printed and counted. A second gate renders each file's first page in every library and excludes files whose ink coverage disagrees: a blank page renders instantly and means nothing. Only two files fail certification now, each over a font that lacks a glyph for a code the page draws.

Compare the rows against each other, not against another machine's numbers.

Reproduce with [`benchmarks/bench_render.py`](benchmarks/README.md).

</details>

### Scanned documents

Scans are the other half of the world's PDFs: one full-page bilevel image per page, JBIG2- or CCITT-coded, no text operators. A 544-page JBIG2 book (1994 × 2832 samples per page) rasterized to PNG at 1:1:

| Library | pages/sec | Ink on page 1 |
|---|--:|--:|
| pdfboss | 66.4 | 4.71% |
| pypdfium2 | 56.5 | 4.85% |
| pdfplumber (via pdfium) | 56.1 | 4.87% |
| PyMuPDF | 55.6 | 4.82% |

**pdfboss is the fastest of the four, about 18% ahead of the C-backed renderers**, and the only one of them with no C in it.

<details>
<summary><strong>Where the time goes, and why the ink column matters</strong></summary>

All four are timed in one pass. The absolute numbers vary by half as the machine warms and cools — compare the four rows against each other, not against another machine's numbers.

What is left is the codec itself. Four fifths of the time goes to the JBIG2 arithmetic decoder and the context formation that feeds it. That part is a serial dependency chain: every decision needs the interval state the previous one wrote, and every pixel's context contains the pixels just decoded. It neither vectorizes nor parallelizes. The rest was arithmetic that did not need doing: expanding a packed scan into eight times its size in RGBA before sampling a fraction of it, blending opaque pixels through an alpha formula that returns them unchanged, and walking bitmaps a pixel at a time where a row of bytes would do.

The ink column is what makes the timings mean anything. A library that cannot decode a scan's codec usually hands back a blank page instead of raising, and a blank page benchmarks superbly. Agreeing coverage says all four decoded the same picture. They do not agree pixel for pixel, because each library downsamples 1994 × 2832 samples onto a 462 × 663 page with its own resampling.

Reproduce with [`benchmarks/bench_scans.py`](benchmarks/README.md).

</details>

### In the browser

[pdfarena](https://pdfarena.tahrioui.de) races pdfboss against hayro, pdf.js and PDFium on any PDF you drop in, with pdfboss and hayro compiled to WebAssembly. Each engine renders in its own web worker, the stopwatch wraps only the render call, and every challenger is pixel-diffed against pdf.js as the reference. Nothing gets uploaded; the whole benchmark runs in your browser.

## What's inside

Ten crates, one implementation: a from-scratch core with its own JPEG 2000, JBIG2, CCITT and ICC codecs, an anti-aliased rasterizer, layout analysis to plain text and Markdown, async range-fetching I/O, a CLI and TUI, and PyO3 bindings.

<details>
<summary><strong>Crate map</strong></summary>

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

</details>

<details>
<summary><strong>Everything supported, in one breath</strong></summary>

**Supported:** classic, stream, and hybrid cross-references with recovery scanning · object streams · FlateDecode, LZWDecode, ASCII85Decode, ASCIIHexDecode, RunLengthDecode + PNG/TIFF predictors · DCTDecode (JPEG) images · JPXDecode (JPEG 2000) images: JP2 containers and raw codestreams, every progression order, both wavelets, palettes, and `/SMaskInData` alpha (ITU-T T.800) · CCITTFaxDecode scans: Group 3 one-dimensional, Group 3 mixed and Group 4 coding (ITU-T T.4/T.6) · JBIG2Decode scans: the full segment type table — generic, text, halftone and refinement regions, immediate or intermediate, symbol and pattern dictionaries, custom code tables, arithmetic- or Huffman-coded with the MMR variants throughout — with or without `/JBIG2Globals` · Standard-handler decryption: RC4 and AES-128/256, with the user or owner password (`--password`, `password=`; the empty user password opens transparently) · page-tree attribute inheritance · text extraction with `ToUnicode` and WinAnsi/MacRoman/Standard encodings · Markdown output with headings, lists, emphasis and pipe/HTML tables inferred from the page layout and from drawn table borders · rasterization of paths, fills (nonzero & even-odd), strokes, transforms, clipping, all blend modes (separable and non-separable), soft masks (image `/SMask`, stencil and color-key `/Mask`, and Luminosity/Alpha `/SMask` groups in `/ExtGState`), image/form XObjects, all seven shading types (the `sh` operator and shading-pattern fills and strokes: function-based, axial and radial through sampled, exponential, stitching and PostScript-calculator functions, plus Gouraud triangle meshes and Coons/tensor patch meshes), tiling patterns (colored and uncolored, each cell run as its own content stream), annotation normal appearances (`/AP` `/N`, with `/AS` state selection), `ICCBased` colour through the embedded profile (ICC.1:2010 v2/v4, matrix/TRC, grayTRC, and `A2B0` lookup transforms) and the CIE `CalRGB`/`CalGray`/`Lab` families through XYZ, and the glyph outlines of every embedded font program (TrueType, CFF, Type1, Type3), with optional substitution for non-embedded simple fonts · lazy element iteration over physical (objects, xref sections, trailer, with byte spans) and logical (pages, fonts, images, annotations, content operators) elements.

</details>

## Limitations

Rendering is lenient and it says so: content pdfboss cannot read is skipped so the rest of the page still rasterizes, and every dropped or approximated item lands in a report — `pdfboss render` warns on stderr, the TUI raises a notice, and the libraries return it through `render_page_reporting` (Rust) and `Page.render_reporting()` (Python). The whole not-yet-supported list is two faces: `/Symbol` and `/ZapfDingbats` have no license-clean substitute, so they stay blank rather than borrowing an unrelated face's glyphs.

<details>
<summary><strong>The full accounting: fonts, CMaps, JBIG2, colour, JPX, layers</strong></summary>

Glyph painting is staged in tiers, selected with `--fonts`. The default, `all-embedded`, paints every embedded font program (TrueType, CFF, Type1 and Type3). `embedded-only` restricts that to TrueType. `full` additionally substitutes a replacement face for a **non-embedded** simple font, from either a directory you supply or the compiled-in OFL Croscore set (behind the `substitute-fonts` feature). Standard-14 advance widths come from the Adobe Core-14 AFM tables when a substitute is used, behind the PDF's own `/Widths`.

Type0 `/Encoding` CMaps resolve — the predefined ISO 32000 Table 118 CJK set is compiled in (behind the `predefined-cmaps` feature, on by default in the CLI and the wheel), embedded CMap streams parse the same way, widths key on the mapped CID, vertical text (`WMode` 1) advances by `/W2`/`/DW2` with the default position vector, and extraction maps CIDs to Unicode through the character collection when `/ToUnicode` is absent; deferred: vertical runs still extract as horizontal-schema spans, one per show operator, with x/y at the glyph origin.

A bold *sans* substitute is not visually distinct from regular weight. Text a tier leaves unpainted still advances — through the PDF's own `/Widths`, or the Adobe Core-14 AFM tables for a standard-14 face — so everything painted around it stays where the page put it.

`JBIG2Decode` covers the embedded stream format end to end: generic regions (all four templates, with TPGDON, arithmetic or MMR-coded), symbol dictionaries and text regions in both the arithmetic and the Huffman variant — refinement/aggregate-coded symbols and refined instance placements included — pattern dictionaries and halftone regions, generic refinement regions (both templates, with TPGRON) refining either the page or a retained intermediate region, intermediate regions of every type, and custom code table segments. Nothing in the standard's segment type table is refused any more; a malformed or truncated stream still fails with a message naming what was wrong instead of rendering a blank.

Colour converts to sRGB. `ICCBased` spaces parse their embedded profile (v2 and v4; matrix/TRC and grayTRC models, and `A2B0` lookup pipelines): a profile equivalent to sRGB keeps the exact device-RGB path, others transform per colour with Bradford adaptation from the D50 connection space, and a profile that will not parse falls back to the `/N` channel-count reduction. `CalRGB`, `CalGray`, and `Lab` convert through CIE XYZ the same way. Only a profile's default transform is used — rendering intents are not switched — and `DeviceN` keeps a tint approximation.

`JPXDecode` implements ITU-T T.800 (JPEG 2000 Part 1); what it approximates it reports as a render warning rather than passing off silently. ICC profiles embedded in the JPEG 2000 container are interpreted through the same ICC engine as `ICCBased` colour — a profile equivalent to sRGB or device gray keeps the exact device path, others transform per sample — and only a profile that will not parse falls back to the channel-count approximation. sYCC converts with the exact IEC 61966-2-1 Amd. 1 inverse. Part 2 (ISO/IEC 15444-2) extensions are tolerated in the container but not decoded. Every output sample is normalized to 8 bits per channel with round-to-nearest, so sources deeper than 8 bits (the spec allows up to 38) still land on an 8-bit output grid.

Optional content groups (PDF layers, ISO 32000 §8.11) are honored per the document's default configuration: rendering and text extraction skip layers it turns off, counting them on the reports' `hidden` counters.

</details>

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
