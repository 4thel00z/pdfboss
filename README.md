<h1 align="center">pdfboss</h1>

<p align="center">
  <strong>A PDF engine written from scratch in Rust: parse, extract text and images, rasterize to PNG, create PDFs. One core, a CLI, and pythonic bindings.</strong>
</p>

<p align="center">
  <a href="https://github.com/4thel00z/pdfboss/actions/workflows/ci.yaml"><img src="https://github.com/4thel00z/pdfboss/actions/workflows/ci.yaml/badge.svg" alt="CI"></a>
  <a href="https://github.com/4thel00z/pdfboss/actions/workflows/python-ci.yml"><img src="https://github.com/4thel00z/pdfboss/actions/workflows/python-ci.yml/badge.svg" alt="python-ci"></a>
  <a href="https://4thel00z.github.io/pdfboss/"><img src="https://img.shields.io/badge/docs-book-blue?logo=mdbook&logoColor=white" alt="Documentation"></a>
  <a href="https://pypi.org/project/pdfboss/"><img src="https://img.shields.io/pypi/v/pdfboss?logo=pypi&logoColor=white" alt="PyPI"></a>
  <img src="https://img.shields.io/badge/rust-2021-000000?logo=rust&logoColor=white" alt="Rust 2021">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="MIT OR Apache-2.0"></a>
</p>

<p align="center">
  <a href="https://4thel00z.github.io/pdfboss/">Docs</a> ·
  <a href="https://pypi.org/project/pdfboss/">PyPI</a> ·
  <a href="https://crates.io/crates/pdfboss-cli">crates.io</a> ·
  <a href="#benchmarks">Benchmarks</a>
</p>

---

Reading a PDF should not require a C library. pdfboss is a clean-room reader built from the ISO 32000 specification: safe Rust, no C dependencies, no bindings to another engine, one core behind the CLI and the native Python extension. It is a **lenient reader** — real-world files are damaged, so it reconstructs broken cross-reference tables, tolerates wrong stream lengths, and skips garbage operators instead of refusing.

## Highlights

- **Clean-room engine** — implemented from the ISO 32000 specification in safe Rust: no C dependencies, no bindings to another engine.
- **Fastest text extraction measured** — 6,700 pages/s over a 40-file real-world corpus, about 15× the C-backed PyMuPDF ([benchmarks](#benchmarks)).
- **Its own codecs** — JPEG 2000, JBIG2, CCITT and ICC are decoded in-tree, implemented from their specifications, not linked in.
- **Embedded-image extraction** — every image a page draws, at native size, alpha applied, from the CLI, Python and Rust ([example](#extract-embedded-images)).
- **PDF creation** — document structs, a canvas painter and a COS writer with deterministic output; CommonMark+GFM composes into CSS-themed PDFs from the CLI, Python and Rust ([example](#create-pdfs)).
- **Async, range-fetching I/O** — documents open over files or `http(s)://` URLs and fetch only the byte ranges they need, never the whole file.
- **Lenient, and it says so** — broken cross-references are reconstructed and unreadable content is skipped, with every dropped or approximated item reported.
- **Terminal explorer** — `pdfboss tui`: element tree, object inspector, hex view, page and Markdown previews.

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
pdfboss images  report.pdf                 # extract embedded images as native-size PNGs
pdfboss tui     report.pdf                 # interactive terminal explorer
pdfboss create blank  -o out.pdf --pages 3    # new PDF: empty pages
pdfboss create text   notes.txt -o out.pdf    # new PDF: word-wrapped text
pdfboss create images a.png b.jpg -o out.pdf  # new PDF: one page per image
pdfboss create md     notes.md -o out.pdf     # new PDF: markdown composed with a CSS theme
```

```python
import pdfboss

doc = pdfboss.Document("report.pdf")       # or Document(data=raw_bytes)
text = doc.extract_text()
md   = doc.extract_markdown()              # headings, lists and tables inferred from layout
png  = doc[0].render(scale=2.0)            # PNG bytes
imgs = doc[0].extract_images()             # embedded images: .data (PNG), .width, .height
pdf  = pdfboss.md.to_pdf(open("notes.md").read())  # markdown -> themed PDF bytes
```

<details>
<summary><strong>More: explorer subcommands, async Python, Rust</strong></summary>

Explorer subcommands. Each except `obj` accepts a local path or an `http(s)://` URL, fetched in ranges and never downloaded whole:

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
    print(element.kind, element.value())
```

Rust — the library crates are on crates.io (`cargo add pdfboss-core pdfboss-text pdfboss-output pdfboss-render pdfboss-write pdfboss-markdown pdfboss-aio pdfboss-tui`):

```rust,no_run
use pdfboss_core::Document;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let page = doc.page(0)?;

    let text = pdfboss_output::extract_text(&doc, &page)?;
    let markdown = pdfboss_output::extract_markdown(&doc)?;
    println!("{text}\n{markdown}");

    let pixmap = pdfboss_render::render_page(&doc, &page, 2.0)?;
    pixmap.save_png("page.png")?;

    let images = pdfboss_render::extract_page_images(&doc, &page)?;
    println!("{} embedded images", images.len());
    Ok(())
}
```

</details>

## Extract embedded images

`pdfboss images`, `Page.extract_images()` and `pdfboss_render::extract_page_images` decode every image a page draws — inline images included — at the image's own pixel dimensions, in drawing order, with `/SMask` alpha applied. An image drawn twice appears twice, and stencil masks (`/ImageMask true`) paint a fill color rather than carrying pixels of their own, so they are skipped.

Real pages draw one-pixel strips and spacers; a size filter keeps the pictures:

```python
import pdfboss

doc = pdfboss.Document("report.pdf")
for index in range(len(doc)):
    for n, image in enumerate(doc[index].extract_images()):
        if image.width < 64 or image.height < 64:
            continue
        with open(f"page-{index}-img-{n}.png", "wb") as out:
            out.write(image.data)
```

The same walk from Rust, one `Pixmap` per image:

```rust,no_run
use pdfboss_core::Document;
use pdfboss_render::extract_page_images;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    for index in 0..doc.page_count() {
        let page = doc.page(index)?;
        for (n, image) in extract_page_images(&doc, &page)?.iter().enumerate() {
            if image.width < 64 || image.height < 64 {
                continue;
            }
            image.save_png(format!("page-{index}-img-{n}.png"))?;
        }
    }
    Ok(())
}
```

## Create PDFs

`pdfboss-write` is the write-side twin of the reader: plain structs compose the document (`Pdf`, `Page`, `PageSize`), a `Canvas` paints content, and a COS-level writer serializes it. Output is deterministic — the same input produces byte-identical files, and the crate never reads clocks or randomness, so dates appear only when supplied.

```rust,no_run
use pdfboss_write::{Color, ImageData, Page, PageSize, Pdf, Standard14};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Page::new(PageSize::A4);
    let canvas = &mut page.canvas;

    canvas.text("Quarterly report", 72.0, 760.0, Standard14::HelveticaBold, 24.0)?;

    canvas.set_fill(Color::Rgb(0.13, 0.45, 0.85));
    canvas.rect(72.0, 744.0, 451.0, 4.0);
    canvas.fill();

    let chart = canvas.add_image(ImageData::png(&std::fs::read("chart.png")?)?);
    canvas.draw_image(chart, 72.0, 480.0, 320.0, 240.0);

    let pdf = Pdf {
        pages: vec![page],
        ..Pdf::default()
    };
    pdf.save("report.pdf")?;
    Ok(())
}
```

The CLI covers the common shapes as one-liners:

```bash
pdfboss create blank  -o blank.pdf --pages 3 --size letter
pdfboss create text   notes.txt -o notes.pdf --font times-roman --font-size 12
pdfboss create images scan1.png photo.jpg -o scans.pdf
```

And CommonMark+GFM composes straight into a themed, paginated PDF — headings, lists, tables, code blocks, clickable links and images — deterministically, from all three surfaces. A theme is a small CSS subset over element-type selectors, overlaid on the built-in default; the Helvetica, Times and Courier families are available, and characters outside them are replaced with `?` and reported:

```css
body { font-family: times; font-size: 10.5pt; color: #222; }
h1   { font-family: helvetica; font-size: 2.2em; color: #a33; }
code { font-family: courier; background-color: #eee; }
pre  { background-color: #eee; padding: 8pt; }
```

```python
from pathlib import Path

import pdfboss

pdf = pdfboss.md.to_pdf(
    Path("notes.md").read_text(),
    theme=Path("theme.css").read_text(),   # CSS source text, not a path
    size="letter",
)
Path("notes.pdf").write_bytes(pdf)
```

```bash
pdfboss create md notes.md -o notes.pdf --theme theme.css --size letter
```

In Rust, `pdfboss_markdown::to_pdf` returns the composed `Pdf` value plus a report of anything sanitized, ready for `save`/`to_bytes` or further canvas work.

## Benchmarks

### Text and parsing

Against other Python PDF libraries over 40 real-world PDFs (pages/sec, higher is faster):

<p align="center">
  <img src="https://raw.githubusercontent.com/4thel00z/pdfboss/main/benchmarks/results.png" alt="pdfboss vs. Python PDF libraries" width="100%">
</p>

**pdfboss is the fastest library measured on both operations, including against the C-backed PyMuPDF and the Rust-backed pdf_oxide**: 6,850 pages/s extracting text against PyMuPDF's 450 (about 15×) and pdf_oxide's 290 (about 23×), and 405,000 pages/s opening + parsing against pdf_oxide's 190,000 (about 2.1×).

<details>
<summary><strong>Method and fine print</strong></summary>

Best-of-3 per file, aggregated over the files every library handled; measured with pdfboss 0.22.0 on an Apple M3 Pro, every table on this page from one session. The pure-Python readers are roughly 85× to 380× slower on extraction. Since 0.9.0, `doc.extract_text()` spreads pages across cores, which widened the gap over the sequential libraries from the 7× measured before that landed; since 0.19.0 every span also carries its style (font, weight, decorations, color), which costs the extraction rows a few percent against older tables. Lazy page-tree loading means opening a document reads only its declared page count instead of parsing every page dictionary up front. Opening is close to free, so the ratio says more about what the others do eagerly than about pdfboss. Rendering is compared in its own section below, restricted to the files pdfboss provably rasterizes completely — timing it against full renderers on the rest would credit it for work it skips.

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
| **pdfboss** (`md`) | **0.882** | Markdown | **0.15s** |
| **pdfboss** | **0.868** | plain text | **0.15s** |
| markitdown 0.1.5 | 0.844 | Markdown | 16.17s |

<details>
<summary><strong>What the score is made of, and how it was measured</strong></summary>

Per document, the plain-text output beats pdf-inspector's NID on 105 of the 200 files, ties on 23 and loses on 72. The losses concentrate in table regions, where structured output matches the ground truth more closely than flowed text can. On the benchmark's combined metric the Markdown adapter scores 0.820 (reading order 0.882, headings and lists 0.702, table structure 0.622). It detects tables from column gaps and from drawn borders, so bordered grids and boxed lists without column gaps are found too. Two-column layouts are read column-major. Justified text keeps its word spacing. Ligatures and small-caps variants decode through the full Adobe Glyph List conventions.

Quality rows come from the benchmark's own evaluator over all 200 documents. The two pdfboss timings were measured together in one session on an Apple M3 Pro under the benchmark's protocol: median of five single-process runs after a warm-up, wheel built from main. pdf-inspector was measured the same way on the same machine in an earlier session. The other engines' timings are the ones [published with the corpus](https://github.com/firecrawl/opendataloader-bench/tree/abi/pdf-parser-benchmark-results) from an Apple M4 Pro. Read them as order-of-magnitude context, not a same-machine race.

</details>

### Rendering

A renderer that skips work looks fast, so every file is certified before the stopwatch starts: any page that reports dropped or approximated content excludes its file, and an ink-coverage gate across libraries catches work skipped silently. 38 of the 40 files (888 pages) certify:

| Library | pages/sec |
|---|--:|
| pdfboss | 144.5 |
| pypdfium2 | 121.3 |
| pdfplumber (via pdfium) | 102.0 |
| PyMuPDF | 89.5 |

pdfboss rasterizes the mixed corpus fastest — about 19% ahead of pdfium itself — with no C in it.

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
| pdfboss | 65.4 | 4.71% |
| pypdfium2 | 57.4 | 4.85% |
| pdfplumber (via pdfium) | 56.5 | 4.87% |
| PyMuPDF | 54.6 | 4.82% |

**pdfboss is the fastest of the four, about 14% ahead of the C-backed renderers**, and the only one of them with no C in it.

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

Fourteen crates, one implementation: a from-scratch core with its own JPEG 2000, JBIG2, CCITT and ICC codecs, an anti-aliased rasterizer, layout analysis to plain text and Markdown, a deterministic PDF writer with CSS-themed Markdown composition, async range-fetching I/O, a CLI and TUI, and PyO3 bindings.

<details>
<summary><strong>Crate map</strong></summary>

| Crate | Responsibility |
|---|---|
| `pdfboss-core` | Tokenizer, object model, stream filters, cross-references, object streams, document & page tree, content-stream operators |
| `pdfboss-text` | Simple and CID/Type0 fonts, standard encodings, `ToUnicode` CMaps, positional text spans |
| `pdfboss-encoding` | Shared font-encoding tables (WinAnsi/MacRoman/Standard, ISO 32000 Appendix D) and glyph-name-to-Unicode mappings, consumed by the text and render crates |
| `pdfboss-output` | Layout analysis over those spans (lines, columns, headings, lists, tables, repeated page headers), rendered as plain text or Markdown |
| `pdfboss-jpx` | JPEG 2000 decoder for `JPXDecode` image streams, implemented from ITU-T T.800 |
| `pdfboss-icc` | ICC profile parser and colour transform to sRGB, implemented from ICC.1:2010 |
| `pdfboss-render` | Anti-aliased vector rasterizer (paths, fills, strokes, clipping, color, images, glyph outlines) to RGBA/PNG, plus embedded-image extraction |
| `pdfboss-write` | PDF creation: COS object writer, content canvas, link annotations and document assembly, deterministic output |
| `pdfboss-style` | CSS-subset themes for document composition |
| `pdfboss-markdown` | CommonMark+GFM composed into themed PDFs |
| `pdfboss-aio` | Async I/O: range-fetching document access over files or HTTP, without reading the whole file |
| `pdfboss-cli` | The `pdfboss` command-line tool |
| `pdfboss-tui` | Interactive terminal explorer (`pdfboss tui`): element tree, object inspector, hex view, page and Markdown previews — built on `pdfboss-aio` |
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

## Documentation

The [pdfboss book](https://4thel00z.github.io/pdfboss/) covers installation, guides for every surface (text, Markdown, styled spans, rendering, image extraction, creation, Markdown-to-PDF composition, async and remote documents, the explorer, encryption) and CLI/Python/Rust reference chapters. It is built with mdBook from [`docs/`](docs/) and deployed through GitHub Pages. Per-crate Rust API documentation is on [docs.rs](https://docs.rs/pdfboss-core).

## Development

```bash
cargo test --workspace          # Rust test suite
cargo clippy --workspace --all-targets -- -D warnings
maturin develop                 # build the Python extension into your venv
pytest                          # Python integration tests
mdbook serve docs               # live-preview the documentation book
```

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you shall be dual-licensed as above, without any additional terms or conditions.
