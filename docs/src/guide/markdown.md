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
- **Tagged PDFs**: under structure-tree reading order the tags decide the
  blocks on every page the tree reaches (ISO 32000-1 14.8.4.3): H1 to H6 are
  headings at that level, H a heading as deep as its Part, Art and Sect
  elements nest, L a list with one item per LI and the Lbl text as the
  marker (read as a number, a letter or a Roman numeral when the list's
  `ListNumbering` attribute names that numbering system, an unlabelled item
  continuing the count), Table a table with a row per TR and a cell per TH
  or TD spanning
  what its `ColSpan` and `RowSpan` attributes say, and every other
  block-level element a paragraph; a Caption or a TOC entry is a paragraph of
  its own, the text of Figures, Formulas and Forms is laid out by the
  heuristics below apart from the text around it, and the paragraphs of a
  BlockQuote render as a block quote. Content the tree does not reach still
  goes through the heuristics below.
- **Page headers, footers and page numbers**: a page's first or last line,
  repeated near-verbatim at the same height on at least half the pages (three at
  minimum), is tagged
  as a running page header or footer; a line that is nothing but a page number is
  tagged without any repetition required. Content the producer marked as a
  pagination artifact (ISO 32000-1 14.8.2.2: a running head, a folio, a
  watermark) is tagged on the producer's word, with no repetition needed.
  Tagged lines are dropped from the Markdown output.

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

### Layout blocks

`layout` returns the structure the Markdown is rendered from instead of the
text: per page, a list of `LayoutBlock` objects in reading order, each with its
`kind` (`"heading"`, `"paragraph"`, `"list"`, `"table"`), its `bbox` in PDF user
space (y-up, like `Span.bbox`), its plain `text`, a heading's `level`, a
paragraph's `role` (`"body"`, `"quote"`, `"page_header"`, `"page_footer"`), a
list's `items` (each with its own `marker`, `text` and `bbox`) and a table's
`rows` of cells.

```python
for page in doc.layout():               # every page, headings ranked globally
    for block in page:
        print(block.kind, block.bbox, block.text[:40])

blocks = doc[3].layout()                # one page, headings ranked per page
first_two = doc.layout(pages=[0, 1])    # document-wide ranking, two pages returned
```

The thresholds that cut the blocks are keyword arguments, defaulting to the
values `extract_markdown` uses. Callers that need block boxes cut differently
tune them per call:

```python
blocks = doc.layout(
    paragraph_gap=1.6,                  # break paragraphs at 1.6 line pitches
    paragraph_pitch="lower_half_median",  # pitch from the tighter half of the steps
    header_footer_min_pages=2,
)
```

Some producers draw a line twice at the same position, for example over a fresh
white box or offset by a fraction of a point to fake bold. Text, Markdown and
`layout()` read such a line once: before layout, a span that repeats an earlier
span on the page (same text, size and rotation, origin within `redraw_tolerance`
of the size, default 0.1, and starting in the first half of that span) is
dropped, so the copy counts neither in the line nor in the heading size ranking.
`redraw_tolerance=0` keeps every copy. `spans()` always reports every draw.

The stubs list every keyword with its default and meaning. `AsyncDocument.layout`
and `AsyncPage.layout` are the coroutine twins. In Rust the same blocks come from
`extract_layout_opts` and `extract_page_layout_opts`, the thresholds from
`LayoutOptions`.

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
`*italic*` inside paragraphs and list items, and under structure-tree reading
order text tagged as a `Code` element renders as inline code between backticks.
Headings drop emphasis markers: a heading is already the strongest thing on the
page. Blocks are separated by a blank
line, across page boundaries too, so the document reads as one continuous Markdown
file. For the raw style information itself, see [Styled spans](./spans.md); for the
reverse direction (Markdown composed into a PDF), see
[Markdown to PDF](./md-to-pdf.md).
