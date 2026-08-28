---
name: pdfboss
description: Use when reading, extracting, rendering, creating, or exploring PDF files with pdfboss, the from-scratch Rust PDF engine with a CLI and Python bindings. Triggers include extracting text or Markdown from a PDF, rasterizing pages to PNG, pulling embedded images, composing a new PDF (blank, text, images, Markdown, TOML manifest, or the pdfboss.write API), inspecting PDF internals (objects, xref, hexdump, jq-style queries), reading PDFs over HTTP without downloading them whole, and opening encrypted PDFs.
---

# pdfboss

A PDF engine written from scratch in safe Rust: parse, extract text and Markdown, rasterize to PNG, extract embedded images, and create PDFs. One core behind the `pdfboss` CLI and the `pdfboss` Python package. Clean-room from ISO 32000, no C dependencies. The reader is lenient: broken cross-reference tables are reconstructed, wrong stream lengths tolerated, garbage operators skipped, and every dropped or approximated item is reported instead of silently lost.

## Install

```bash
pip install pdfboss            # abi3 wheels, CPython 3.12+, no toolchain needed
pip install 'pdfboss[full]'    # adds the OFL substitute faces (pdfboss-fonts)
cargo install pdfboss-cli      # the `pdfboss` binary
```

## CLI

```bash
pdfboss info    doc.pdf                     # version, page count, sizes, metadata
pdfboss text    doc.pdf --page 2            # omit --page for all pages
pdfboss md      doc.pdf                     # Markdown: headings, lists, tables from layout
pdfboss render  doc.pdf --page 1 -o p.png --scale 2.0
pdfboss images  doc.pdf -o out/             # embedded images as native-size PNGs
pdfboss tui     doc.pdf                     # interactive terminal explorer
```

Every command takes a local path or an `http(s)://` URL; remote files are fetched in byte ranges rather than downloaded whole. Encrypted files take `--password` (an empty user password opens transparently).

Creation:

```bash
pdfboss create blank  -o out.pdf --pages 3
pdfboss create text   notes.txt -o out.pdf
pdfboss create images a.png b.jpg -o out.pdf
pdfboss create md     notes.md -o out.pdf        # CommonMark+GFM, CSS-themable
pdfboss create manifest doc.toml -o out.pdf      # [meta] plus [[page]] text/paragraph/image/link
```

Explorer:

```bash
pdfboss json doc.pdf                 # the document as a JSON value tree (--layout adds page blocks)
pdfboss q    doc.pdf '.header.version'
pdfboss obj  doc.pdf 5               # pretty-print object 5
pdfboss hex  doc.pdf obj:5           # hexdump the file or one element
```

Fonts: `--fonts embedded-only|all-embedded|full`. The default resolves to `full` when substitute faces are available (the compiled-in OFL set or `--font-dir`), otherwise `all-embedded`. Text a tier leaves unpainted still advances, so the rest of the page keeps its layout.

## Python

```python
import pdfboss

doc  = pdfboss.Document("doc.pdf")            # or Document(data=raw_bytes), password=""
text = doc.extract_text()                     # pages fan out across cores
md   = doc.extract_markdown()
png  = doc[0].render(scale=2.0)               # PNG bytes
png, warnings = doc[0].render_reporting()     # warnings list every drop or approximation
images = doc[0].extract_images()              # each: .data (PNG bytes), .width, .height
spans  = list(doc.spans())                    # styled spans: font, weight, color, position

# async, over files or HTTP, range-fetched
doc = await pdfboss.AsyncDocument.open_url("https://example.com/doc.pdf")

# composition: pages and elements join with |, singleton slots raise on duplicates
from pdfboss.write import Pdf, Page, Text, Paragraph, Metadata, Standard14
page = (
    Page(size="a4")
    | Text("Title", at=(72, 770), font=Standard14.HELVETICA_BOLD, size=28)
    | Paragraph("Body text.", rect=(72, 100, 451, 640))
)
data = (Pdf() | Metadata(title="Title") | page).to_bytes()

# markdown to PDF: returns the file bytes
data = pdfboss.md.to_pdf("# Hello\n\nWorld", theme=None, size="a4")
```

`fonts=` on the render methods defaults to `None`, resolving to `"full"` when `font_dir=` is given or the `pdfboss-fonts` package is importable, else `"all-embedded"`; an explicit `fonts="full"` with no face source raises `ValueError`. The type stubs in `pdfboss/_pdfboss.pyi` are the authoritative signatures.

## Rust

Library crates on crates.io: `pdfboss-core` (the reader), `pdfboss-text`, `pdfboss-output` (plain text and Markdown), `pdfboss-render` (rasterizer; `RenderOptions` fields `glyph_painting`, `substitutes`, `oc`, `cache`), `pdfboss-write` (creation), `pdfboss-markdown` (Markdown to PDF), `pdfboss-aio` (async range-fetching I/O), `pdfboss-jpx` and `pdfboss-icc` (its own codecs), `pdfboss-tui`.

## Gotchas

- Rendering never fails on unreadable content; when fidelity matters, use `render_reporting` and check the warnings instead of assuming a clean render.
- `/Symbol` and `/ZapfDingbats` have no license-clean substitute and stay blank at every tier.
- Scanned PDFs (JBIG2, CCITT) carry no text layer: `text` and `md` return little or nothing there; render the pages instead.
- `extract_markdown` drops repeated page headers and footers by design.
- A whole-document Rust render walk should share one `RenderCache` through `RenderOptions::cache` so fonts and ICC profiles load once.

## Links

- User guide: https://4thel00z.github.io/pdfboss/
- Site and benchmarks: https://pdfboss.dev
- Race it in a browser: https://pdfarena.tahrioui.de
- Repository: https://github.com/4thel00z/pdfboss
