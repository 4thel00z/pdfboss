# Quickstart

One taste of each surface. `report.pdf` stands in for any PDF of yours;
[Installation](./installation.md) covers getting the binary, the wheel and the
crates.

## CLI

```bash
pdfboss info    report.pdf                    # version, page count, page sizes, metadata
pdfboss text    report.pdf --page 2           # extract text (omit --page for all pages)
pdfboss md      report.pdf                    # markdown: headings, lists, tables from layout
pdfboss render  report.pdf --page 1 -o page.png --scale 2.0
mkdir out
pdfboss images  report.pdf -o out/            # embedded images as native-size PNGs
pdfboss create text notes.txt -o notes.pdf    # a new PDF from a word-wrapped text file
pdfboss create md   notes.md -o notes.pdf     # markdown composed with a CSS theme
```

`render` prints what it wrote (`wrote page.png (1224 x 1584 px)`) and warns on
stderr about anything it had to drop; `images` names every file it writes,
`out/page-1-image-1.png` style. Page numbers are 1-based on the command line.

## Python

```python
from pathlib import Path

import pdfboss

doc = pdfboss.Document("report.pdf")
print(doc.page_count, "pages, PDF", doc.version)

text = doc.extract_text()          # all pages, form-feed separated
markdown = doc.extract_markdown()  # headings, lists, tables from layout

page = doc[0]
Path("page.png").write_bytes(page.render(scale=2.0))

for image in page.extract_images():
    print(image.width, image.height, len(image.data))

pdf = pdfboss.md.to_pdf(Path("notes.md").read_text())  # markdown -> PDF bytes
```

`Document` also opens from memory (`Document(data=raw_bytes)`), pages index
0-based with negative indexes from the end, and `render` returns PNG bytes
directly. `extract_images` yields each image the page draws at its native
pixel size, PNG-encoded. `pdfboss.md.to_pdf` composes CommonMark+GFM into a
themed PDF ([Markdown to PDF](./guide/md-to-pdf.md)); `pdfboss.write` composes
pages, elements and document slots with `|`, the same vocabulary the CLI and
the `pdfboss-write` crate expose ([Creating PDFs](./guide/creating.md)).

## Rust

With `pdfboss-core`, `pdfboss-output` and `pdfboss-render` added:

```rust,no_run
use pdfboss_core::Document;
use pdfboss_output::ReadingOrder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    println!("{} pages", doc.page_count());

    let page = doc.page(0)?;
    let text = pdfboss_output::extract_text(&doc, &page, ReadingOrder::Content)?;
    println!("{text}");

    let pixmap = pdfboss_render::render_page(&doc, &page, 2.0)?;
    pixmap.save_png("page.png")?;
    Ok(())
}
```

`render_page` returns an RGBA `Pixmap` (`width`, `height`, `data`); `save_png`
and `encode_png` turn it into a file or bytes. Page indexes are 0-based, as in
Python; only the CLI counts from 1.

## Next steps

- [Extracting text](./guide/text.md) and [Markdown output](./guide/markdown.md)
  for layout analysis details.
- [Rendering pages](./guide/rendering.md) for scale, font tiers and PNG
  compression.
- [Extracting images](./guide/images.md) for what counts as an embedded image.
- [Creating PDFs](./guide/creating.md) for canvas painting, composed pages
  and document slots.
- [Markdown to PDF](./guide/md-to-pdf.md) for themed CommonMark+GFM
  composition.
- [Async and remote documents](./guide/async.md) for `AsyncDocument` and HTTP
  range fetching.
