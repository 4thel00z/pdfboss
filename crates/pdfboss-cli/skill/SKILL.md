---
name: pdfboss
description: Use when reading, extracting, rendering, creating, or exploring PDF files with pdfboss, the from-scratch Rust PDF engine with a CLI and Python bindings. Triggers include extracting text or Markdown from a PDF, rasterizing pages to PNG, PPM, BMP or JPEG, pulling embedded images, composing a new PDF (blank, text, images, Markdown, TOML manifest, or the pdfboss.write API), watermarking an existing PDF or overlaying one PDF's first page onto every page of another, editing or updating PDF metadata without rewriting the file, merging, splitting or rotating pages, rewriting a document fresh, inspecting PDF internals (objects, xref, hexdump, jq-style queries), reading PDFs over HTTP without downloading them whole, and opening, encrypting or decrypting PDFs.
---

# pdfboss

A PDF engine written from scratch in safe Rust: parse, extract text and Markdown, rasterize to PNG, PPM, BMP or JPEG, extract embedded images, and create PDFs. One core behind the `pdfboss` CLI and the `pdfboss` Python package. Clean-room from ISO 32000, no C dependencies. The reader is lenient: broken cross-reference tables are reconstructed, wrong stream lengths tolerated, garbage operators skipped, and every dropped or approximated item is reported instead of silently lost.

## Install

```bash
pip install pdfboss            # abi3 wheels, CPython 3.12+, no toolchain needed
pip install 'pdfboss[full]'    # adds the OFL substitute faces (pdfboss-fonts)
cargo install pdfboss-cli      # the `pdfboss` binary
```

Coding agents can install this skill with `pdfboss skill install` (writes it into the agent's skill directory; `pdfboss skill print` writes it to stdout) or with `npx skills add 4thel00z/pdfboss`.

## CLI

```bash
pdfboss info    doc.pdf                     # version, page count, sizes, metadata
pdfboss text    doc.pdf --page 2            # omit --page for all pages
pdfboss md      doc.pdf                     # Markdown: headings, lists, tables from layout, pages read in content order
pdfboss text    doc.pdf --reading-order structure-tree   # or geometric; content is the default (text and md)
pdfboss render  doc.pdf --page 1 -o p.png --scale 2.0   # -o extension picks .png/.ppm/.bmp/.jpg; --jpeg-quality 1-100
pdfboss images  doc.pdf -o out/             # embedded images as native-size PNGs
pdfboss meta    doc.pdf -o out.pdf --set title=X --set author=Y   # /Info fields (repeatable --set): title, author, subject, keywords, creator, producer; appends an update, --rewrite for a fresh file instead
pdfboss merge   a.pdf:2-9 b.pdf -o out.pdf              # combine selected pages from several inputs into one fresh document
pdfboss split   doc.pdf -o 'part-%d.pdf' --every 10     # cut into consecutive chunks of pages
pdfboss rotate  doc.pdf -o out.pdf --pages 2,4-9 --by 90   # quarter turns clockwise; appends an update, --rewrite for a fresh file instead
pdfboss overlay doc.pdf mark.pdf -o out.pdf --under        # mark.pdf's first page onto every page, on top by default (--under for beneath); appends an update, --rewrite for a fresh file instead
pdfboss encrypt doc.pdf -o out.pdf --user-password X --allow print,copy   # AES-256 (R6); --owner-password falls back to the user password; --password re-opens an encrypted input to re-encrypt it
pdfboss decrypt doc.pdf -o out.pdf --password X             # remove protection, a fresh plain file
pdfboss rewrite doc.pdf -o out.pdf                      # whole document fresh: recompressed, unreachable objects and old update sections dropped
pdfboss tui     doc.pdf                     # interactive terminal explorer
```

Every command takes a local path or an `http(s)://` URL; remote files are fetched in byte ranges rather than downloaded whole (a server that ignores `Range` costs one full download). When stderr is a terminal, a ranged open draws a two-line coverage minimap of the byte ranges fetched so far, erased once the document is open; non-interactive runs never see it. Encrypted files take `--password` (an empty user password opens transparently).

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

In the TUI, `y` opens a yank menu on the selected element (jq-style query, equivalent CLI command, hexdump, raw bytes, decoded value, object reference), `y e` copies the element and `y m` the current page's Markdown; panes resize with Alt+arrows.

Fonts: `--fonts embedded-only|all-embedded|full`. The default resolves to `full` when substitute faces are available (the compiled-in OFL set or `--font-dir`), otherwise `all-embedded`. Text a tier leaves unpainted still advances, so the rest of the page keeps its layout.

## Python

```python
import pdfboss

doc  = pdfboss.Document("doc.pdf")            # or Document(data=raw_bytes), password=""
text = doc.extract_text()                     # pages fan out across cores
md   = doc.extract_markdown()
tree = doc.extract_text(reading_order=pdfboss.ReadingOrder.STRUCTURE_TREE)   # or "geometric"; default "content"
png  = doc[0].render(scale=2.0)               # PNG bytes; format="ppm"/"bmp" for raw RGB pixels, "jpeg" (quality=90) for lossy
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

# edit metadata on an existing file: appends an incremental update, base bytes untouched
update = pdfboss.write.Update(doc)
update.set_metadata(title="Q3 Report", author="Finance")   # kwarg per /Info field; repeated calls merge
update.save("out.pdf")                             # or update.to_bytes(); encrypted bases refused here

# watermark an existing file: overlay's first page drawn over every page, as an
# incremental update appended to the original bytes (rewrite=True writes a fresh, compressed
# file; under=True draws the overlay beneath the page's own content instead of on top of it)
out_bytes = pdfboss.write.watermark(original_bytes, overlay_bytes, under=True)

# assemble documents: 0-based page lists throughout; merge/split/rewrite always build a fresh file,
# rotate appends an incremental update by default (rewrite=True writes a fresh file instead)
from pdfboss.write import merge, split, rotate, rewrite
combined = merge([a_bytes, (b_bytes, [1, 0])])   # bytes for every page, or (bytes, list[int]) to select/reorder
parts    = split(doc_bytes, every=10)            # consecutive chunks, last one carries the remainder
rotated  = rotate(doc_bytes, 90, pages=[0])
clean    = rewrite(doc_bytes)                    # recompressed, unreachable objects and old update sections dropped

# encrypt/decrypt: AES-256 (R6); owner_password defaults to user_password, both empty raises ValueError
from pdfboss.write import encrypt, decrypt
locked = encrypt(doc_bytes, user_password="X", allow=["print", "copy"])   # allow omitted grants everything
plain  = decrypt(locked, password="X")

# markdown to PDF: returns the file bytes
data = pdfboss.md.to_pdf("# Hello\n\nWorld", theme=None, size="a4")
```

`fonts=` on the render methods defaults to `None`, resolving to `"full"` when `font_dir=` is given or the `pdfboss-fonts` package is importable, else `"all-embedded"`; an explicit `fonts="full"` with no face source raises `ValueError`. `format=` is `"png"` (default), `"ppm"`, `"bmp"` or `"jpeg"`: PPM and BMP are the pixels behind a header (no encode cost, alpha dropped), JPEG is the lossy one with `quality=` 1 to 100. The type stubs in `pdfboss/_pdfboss.pyi` are the authoritative signatures.

## Rust

Library crates on crates.io: `pdfboss-core` (the reader), `pdfboss-text`, `pdfboss-output` (plain text and Markdown), `pdfboss-render` (rasterizer; `RenderOptions` fields `glyph_painting`, `substitutes`, `oc`, `cache`), `pdfboss-write` (creation), `pdfboss-markdown` (Markdown to PDF), `pdfboss-aio` (async range-fetching I/O), `pdfboss-jpx` and `pdfboss-icc` (its own codecs), `pdfboss-tui`.

## Gotchas

- Rendering never fails on unreadable content; when fidelity matters, use `render_reporting` and check the warnings instead of assuming a clean render.
- `/Symbol` and `/ZapfDingbats` have no license-clean substitute and stay blank at every tier.
- Scanned PDFs (JBIG2, CCITT) carry no text layer: `text` and `md` return little or nothing there; render the pages instead.
- `extract_markdown` drops repeated page headers and footers by design.
- A whole-document Rust render walk should share one `RenderCache` through `RenderOptions::cache` so fonts and ICC profiles load once.
- Encrypted output (`encrypt`) differs on every run: the file key, salts and IVs are fresh random each time. An incremental update (`meta`, `rotate`, `overlay` defaults, `Update`) still refuses any encrypted base outright, password-opened or not.

## Links

- User guide: https://pdfboss.dev/docs/
- Site and benchmarks: https://pdfboss.dev
- Race it in a browser: https://pdfarena.tahrioui.de
- Repository: https://github.com/4thel00z/pdfboss
