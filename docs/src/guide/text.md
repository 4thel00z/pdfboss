# Extracting text from PDFs

Text extraction turns a page's positioned glyphs back into readable text: spans are
grouped into lines, lines are joined with `\n`, and spaces are inserted at horizontal
gaps. Reading order follows the content stream: a typeset document writes each
column whole before the next begins, so the text comes out column by column, and a
figure caption or a footnote reads where the producer placed it. Geometry corrects
the streams that write across two columns row by row (a page with a clear gutter
still reads column-major) and takes over entirely when a stream was not written in
reading order at all, which then reads top to bottom. Whole-document extraction
joins pages with a form feed (`\f`). For structured output (headings, lists,
tables), see [Markdown output](./markdown.md); for the spans themselves, with
fonts, sizes and positions, see [Styled spans](./spans.md).

## CLI

```bash
pdfboss text report.pdf
```

prints every page, separated by form feeds. `--page` selects one page, 1-based:

```bash
pdfboss text --page 4 report.pdf
```

Content that cannot be read is reported on stderr, one warning per skipped stream,
with the same 1-based page numbers:

```text
warning: page 17: skipped a form XObject (form limit exceeded)
```

stdout carries only the extracted text, so the output stays safe to pipe. Encrypted
files take `--password`. See [Encrypted documents](./encryption.md).

## Python

```python
from pdfboss import Document

doc = Document("report.pdf")

text = doc.extract_text()           # all pages, joined by "\f"
page_text = doc[3].extract_text()   # one page (0-based index)
```

`Document.extract_text` fans the pages out across the machine's cores: each worker
thread holds its own fork of the document (the immutable parsed core is shared, the
caches are private), and one font cache serves every worker so each font loads once
per document. Both calls release the GIL while they run, so other Python threads
keep making progress during long extractions.

## Rust

Per page, with [`pdfboss_output::extract_text`](https://docs.rs/pdfboss-output):

```rust,no_run
use pdfboss_core::Document;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    for i in 0..doc.page_count() {
        let page = doc.page(i)?;
        let text = pdfboss_output::extract_text(&doc, &page)?;
        println!("{text}");
    }
    Ok(())
}
```

For a whole document, `pdfboss_core::map_pages` runs a closure over every page in
parallel and returns the results in page order. It fans out over
`std::thread::available_parallelism()` threads, each holding its own document fork;
workers pull page indexes from a shared counter, so pages of uneven cost cannot
strand a fast core behind a slow stripe. Pass one `FontCache` to the `_cached`
variant so fonts load once per document rather than once per page:

```rust,no_run
use pdfboss_core::{map_pages, Document};
use pdfboss_output::FontCache;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let fonts = FontCache::default();
    let outcomes = map_pages(&doc, |doc, page| {
        let (text, _) = pdfboss_output::extract_text_reporting_cached(doc, page, &fonts)?;
        Ok(text)
    });
    let texts = outcomes.into_iter().collect::<Result<Vec<String>, _>>()?;
    println!("{}", texts.join("\u{c}"));
    Ok(())
}
```

Asynchronous callers use `extract_text_with` against any object source. See
[Async and remote documents](./async.md).

## Lenient semantics and reporting

Extraction is lenient the way rendering is: content that will not fetch, decode, or
parse yields no text rather than an error, so one unreadable stream never costs the
rest of the document. `extract_text_reporting` is what keeps that leniency
accountable. It returns the text together with an `ExtractReport`:

```rust,no_run
use pdfboss_core::Document;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let page = doc.page(0)?;
    let (text, report) = pdfboss_output::extract_text_reporting(&doc, &page)?;
    for skip in &report.skipped {
        eprintln!("skipped {} ({})", skip.kind, skip.cause);
    }
    println!("{text}");
    Ok(())
}
```

`report.skipped` names each stream that yielded no text: the kind (the page's own
contents, a form XObject, an unresolvable XObject, a font's CMap encoding) and the
cause (an unsupported filter, a stream that would not read, content that would not
parse, a missing resource, an exhausted form nesting or invocation limit).
`report.hidden` counts
content the document's optional-content configuration turns off; that is configured
behavior, not a loss, so `report.is_complete()` ignores it and is true exactly when
nothing was left out. An empty text with an empty report really is an empty page.
The CLI warnings above are this report, printed. Layers the document's default
optional-content configuration disables are excluded from the text.

## Encodings

Character decoding covers `ToUnicode` CMaps, the WinAnsi, MacRoman and Standard
simple-font encodings, the built-in encoding of an embedded Type 1 program (the base
table of a simple font that names no `/Encoding` of its own, which is how TeX's
symbol and math fonts arrive), and CID-keyed Type0 fonts: embedded `/Encoding` CMap
streams parse, the predefined ISO 32000 CJK CMap set is compiled in (behind the
`predefined-cmaps` feature, on by default in the CLI and the Python wheel), and when
`/ToUnicode` is absent CIDs map to Unicode through the font's character collection.
Glyph names resolve through the full Adobe Glyph List plus the TeX symbol-font names
the list lacks, so ligatures (`fi`, `fl`, …), small-caps variants and math symbols
(`∀`, `∃`, `↦`, `′`) decode to their proper Unicode text.
