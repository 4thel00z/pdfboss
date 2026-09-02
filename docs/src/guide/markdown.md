# PDF to Markdown

Markdown output runs layout analysis over the same positioned spans that
[plain text extraction](./text.md) flattens, and renders the result as CommonMark.
The analysis infers:

- **Headings**: ATX headings (`#` … `######`), ranked by font size. With
  whole-document extraction the sizes are ranked against every page at once, so a
  title page or a chapter opener (all of it larger than body text) is read as
  headings rather than as its own idea of body size. Page-local extraction ranks
  against that page alone; a page whose text is all one size has no heading to
  find. Prefer document-wide extraction whenever the document is at hand.
- **Lists**: bulleted and numbered. Bullets render as `- ` regardless of the
  source glyph; numbered items keep their detected number as `n. `.
- **Tables**: detected both from column gaps and from drawn borders, so bordered
  grids and boxed lists without column gaps are found too. When a table's structure
  is drawn as ruled lines, those borders decide the grid ahead of column occupancy.
  Tables render as pipe tables while every cell stands in one column, and as HTML
  tables as soon as a cell spans several.
- **Two-column pages**: read column-major: the left column top to bottom, then the
  right. The `reading_order` keyword (CLI `--reading-order`) selects content order,
  the structure tree of a tagged PDF, or geometric position; see
  [Reading order](./text.md#reading-order).
- **Page headers, footers and page numbers**: a page's first or last line,
  repeated near-verbatim at the same height on at least half the pages (three at
  minimum), is tagged
  as a running page header or footer; a line that is nothing but a page number is
  tagged without any repetition required. Tagged lines are dropped from the
  Markdown output.

## CLI

```bash
pdfboss md report.pdf
```

`--page` extracts one page, 1-based; heading sizes are then ranked per page rather
than across the document:

```bash
pdfboss md --page 4 report.pdf
```

Warnings for skipped content appear on stderr, exactly as for `pdfboss text`. See
[lenient semantics](./text.md#lenient-semantics-and-reporting).

## Before and after

Page 4 of a physics report carries a cut-flow table. Plain text extraction flattens
it into space-separated lines:

```text
Table 4: Muon RD Branch no. 1
Cuts km2loose acc km2tight acc bipulkm2acc acc
BADRUN 5189813 (-) 5189813 (-) - (-)
ictime − cktbm 3324048 (-) 3324048 (-) - (-)
icbit 3322245 (-) 3322245 (-) - (-)
```

`pdfboss md --page 4 report.pdf` recovers the grid (first rows shown):

```text
Table 4: Muon RD Branch no. 1

| Cuts | km2loose acc | km2tight acc | bipulkm2acc acc |
| --- | --- | --- | --- |
| BADRUN | 5189813 (-) | 5189813 (-) | - (-) |
| ictime − cktbm | 3324048 (-) | 3324048 (-) | - (-) |
| icbit | 3322245 (-) | 3322245 (-) | - (-) |
```

## Python

```python
from pdfboss import Document

doc = Document("report.pdf")

markdown = doc.extract_markdown()       # whole document, headings ranked globally
page_md = doc[3].extract_markdown()     # one page, headings ranked per page
```

`Document.extract_markdown` extracts the pages in parallel and releases the GIL,
like `extract_text`.

## Rust

Whole document, with `pdfboss_output::extract_markdown`:

```rust,no_run
use pdfboss_core::Document;
use pdfboss_output::ReadingOrder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let markdown = pdfboss_output::extract_markdown(&doc, ReadingOrder::Content)?;
    println!("{markdown}");
    Ok(())
}
```

One page, with `extract_page_markdown`:

```rust,no_run
use pdfboss_core::Document;
use pdfboss_output::ReadingOrder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let page = doc.page(0)?;
    let markdown = pdfboss_output::extract_page_markdown(&doc, &page, ReadingOrder::Content)?;
    println!("{markdown}");
    Ok(())
}
```

`extract_markdown_reporting` returns the Markdown together with one
`ExtractReport` per page, in page order (the same accountability contract as text
extraction): unreadable content costs its own text and nothing else, and the report
says what was left out. See
[lenient semantics](./text.md#lenient-semantics-and-reporting) for the report's
shape. Asynchronous callers compose `extract_page_markdown_with` against any object
source. See [Async and remote documents](./async.md).

Emphasis survives into the output: bold and italic runs render as `**bold**` and
`*italic*` inside paragraphs and list items. Headings drop emphasis markers: a
heading is already the strongest thing on the page. Blocks are separated by a blank
line, across page boundaries too, so the document reads as one continuous Markdown
file. For the raw style information itself, see [Styled spans](./spans.md); for the
reverse direction (Markdown composed into a PDF), see
[Markdown to PDF](./md-to-pdf.md).
