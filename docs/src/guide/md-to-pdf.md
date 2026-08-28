# Markdown to PDF

pdfboss composes CommonMark+GFM source into a themed, paginated PDF: the
`pdfboss create md` CLI subcommand, `pdfboss.md.to_pdf` in Python, and
`pdfboss_markdown::to_pdf` in Rust. This is the reverse of
[Markdown output](./markdown.md), which extracts Markdown *from* a PDF; the
composed document reads back through the whole toolkit like any other file.

What the composition covers: headings, paragraphs, bulleted and numbered
lists (nested included), GFM tables with per-column alignment, fenced and
indented code blocks, block quotes, thematic breaks, emphasis, strong,
strikethrough and inline code runs, hyperlinks (emitted as real clickable
`/Link` annotations), and images — PNG or JPEG, detected by content, with
relative paths resolved against a base directory. Raw HTML fragments are
skipped and reported rather than half-rendered.

Two properties worth designing around:

- **Deterministic.** The same markdown, theme and options always produce
  byte-identical output — nothing reads a clock, randomness or the
  environment.
- **Replace-and-report.** Text renders in the standard Helvetica, Times and
  Courier families, which encode as WinAnsi. A character outside that
  encoding is replaced with `?` and tallied in a report naming each replaced
  character; a clean document reports nothing.

## Themes

A theme is a small CSS subset: element-type selectors only, cascading over
the built-in default theme, with inheritance from `body` down. The twenty
selectable elements are `body`, `h1`–`h6`, `p`, `code`, `pre`, `blockquote`,
`ul`, `ol`, `li`, `table`, `th`, `td`, `a`, `del` and `hr`. Properties
include `font-family` (`helvetica`, `times`, `courier`, or the `sans-serif`/
`serif`/`monospace` aliases), `font-size` (`pt`, `px`, `em`, `mm`, `cm`,
`in`), `font-style`, `font-weight`, `color` and `background-color` (named,
`#hex` or `rgb()`), `text-align`, `text-decoration`, `line-height`, and
`margin`/`padding` with their per-side forms. Parse errors are strict and
located — a typo fails with a line and column rather than being ignored.

```css
body { font-family: times; font-size: 10.5pt; color: #222; }
h1   { font-family: helvetica; font-size: 2.2em; color: #a33; }
code { font-family: courier; background-color: #eee; }
pre  { background-color: #eee; padding: 8pt; }
```

## CLI

```bash
pdfboss create md notes.md -o notes.pdf --theme theme.css --size letter
```

`--theme` takes a CSS file (omitted, the built-in default theme applies);
`--size` is `a3`, `a4` (default), `a5`, `letter` or `legal`, and
`--landscape` swaps width and height. Relative image paths in the markdown
resolve against the input file's directory. The result round-trips
immediately:

```bash
pdfboss info themed.pdf     # 1 page, 612 x 792 pt
pdfboss text themed.pdf     # the composed text back out
```

## Python

`pdfboss.md.to_pdf` returns the PDF as bytes. `theme` is **CSS source text,
not a path** — read the file yourself when the theme lives in one. `size`
names the page size case-insensitively, `landscape` swaps the dimensions,
and `base_dir` anchors relative image paths (default: the current
directory).

```python
from pathlib import Path

import pdfboss

theme = Path("theme.css").read_text()
pdf = pdfboss.md.to_pdf(Path("notes.md").read_text(), theme=theme, size="letter")
Path("notes.pdf").write_bytes(pdf)
```

Replacements and skipped raw HTML surface as a single `UserWarning` naming
what changed, so a clean run stays silent and a lossy one is visible without
being fatal:

```python
import warnings

import pdfboss

with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    pdf = pdfboss.md.to_pdf("Snowman ☃ here")
for warning in caught:
    print(warning.message)   # replaced 1 character unavailable in the standard fonts: '☃'×1
```

An unknown `size` or an invalid theme raises `PdfError`.

## Rust

`pdfboss_markdown::to_pdf(markdown, &options)` returns the composed
[`pdfboss_write::Pdf`](./creating.md) — still a value, not yet bytes —
alongside the replace-and-report `Report`. `Options` carries the parsed
`Theme`, the `PageSize` and the image `base_dir`; `Theme::parse` reads CSS
source, `Theme::default_theme()` is the built-in look. Serialize the `Pdf`
with any of the [write paths](./creating.md#writing-the-file):

```rust,no_run
use pdfboss_markdown::{to_pdf, Options, PageSize, Theme};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let markdown = std::fs::read_to_string("notes.md")?;
    let theme = Theme::parse("h1 { font-family: helvetica; color: #a33; }")?;
    let options = Options {
        theme,
        page_size: PageSize::Letter,
        ..Options::default()
    };
    let (pdf, report) = to_pdf(&markdown, &options)?;
    if !report.is_empty() {
        eprintln!("{}", report.summary());
    }
    pdf.save("notes.pdf")?;
    Ok(())
}
```

Because the result is a plain `Pdf`, everything from
[Creating PDFs](./creating.md) applies afterwards: set `metadata`, append
canvas-painted pages of your own, or stream the bytes asynchronously.

## Round trip

Composition and extraction are the two directions of one engine. A document
composed from Markdown reads back with `pdfboss text`, renders with
`pdfboss render`, and — closing the loop — `pdfboss md` re-infers headings,
lists and tables from the composed layout. The composed text uses only
standard-14 faces, so rendering paints it at the `full`
[font tier](./rendering.md#font-tiers).
