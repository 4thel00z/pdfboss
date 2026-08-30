# Creating PDFs

The write side of pdfboss is the `pdfboss-write` Rust crate, the `pdfboss.write` Python module and the `pdfboss create` CLI. This chapter covers two altitudes: the canvas level, where you place every shape, glyph run and image yourself, and the element level one step above it, where `Text`, `Paragraph`, `Image` and `Link` values compose onto pages and document slots carry outlines, attachments, page labels and viewer preferences. Python reaches both: `pdfboss.write` composes elements and slots with `|`, and its draw protocol paints on the canvas directly. To compose a document from CommonMark+GFM source instead, from the CLI, Rust or Python (`pdfboss.md.to_pdf`), see [Markdown to PDF](./md-to-pdf.md). Everything the writer emits uses the same content-stream IR the reader parses, so a created file round-trips through the rest of the toolkit.

## The document model

A document is plain data: `Pdf { metadata, pages, outline, attachments, page_labels, viewer, options }`. The fields are the composition: pages appear in the output in the order of the `Vec`, singleton slots are `Option`s, sequences may stay empty, and `Default` fills everything optional. Each `Page { size, rotation, canvas, content, links }` carries operators painted directly on its `canvas`, composed elements in `content` (lowered onto the canvas at serialization time, after anything painted directly), and its clickable areas: a `LinkAnnotation { rect, target }` marks a rectangle in page user space, emitted as a `/Link` annotation under `/Annots`, whose `LinkTarget` is either `Uri(String)` (a `/URI` action) or `Page(usize)` (a `/GoTo` action with an explicit `/XYZ null null null` destination that keeps the viewer's current position and zoom).

```rust,no_run
use pdfboss_write::{Page, PageSize, Pdf, Standard14};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("Hello from pdfboss", 72.0, 770.0, Standard14::Helvetica, 24.0)?;
    let pdf = Pdf {
        pages: vec![page],
        ..Pdf::default()
    };
    pdf.save("hello.pdf")?;
    Ok(())
}
```

Coordinates are PDF user space: the origin is the bottom-left corner of the page, y grows upward, and one unit is 1/72 inch (a point). `PageSize` offers `A3` (841.89 × 1190.55), `A4` (595.28 × 841.89, the default), `A5` (419.53 × 595.28), `Letter` (612 × 792), `Legal` (612 × 1008) and `Custom { width, height }`; `dimensions()` returns the pair, and `landscape()` swaps it into a `Custom`. `rotation` is a clockwise view rotation in degrees.

Serialization validates rather than guesses. A document with zero pages is an error, and so are a rotation that is not a multiple of 90, a link, bookmark or `open_to` target page out of range, a duplicate attachment name, a page-label set without a range at page 0 or with a duplicate `first_page` or a `start_at` of 0 (numbering starts at 1), and a paragraph whose wrapped lines overflow its rect.

## Canvas

`Canvas` is an imperative painter. Path construction (`move_to`, `line_to`, `curve_to`, `close`) is separate from painting (`fill`, `fill_even_odd`, `stroke`, `close_stroke`, `fill_stroke`, `end_path`), exactly as in PDF content streams. Convenience shapes append complete subpaths: `rect`, `circle` and `ellipse` (four Bézier arcs), and `polygon` over a slice of `pdfboss_core::Point` (fewer than three points appends nothing).

Graphics state follows the same operators: `save`/`restore` push and pop, `transform` concatenates a `pdfboss_core::Matrix` onto the CTM, and `set_line_width`, `set_line_cap`, `set_line_join`, `set_miter_limit` and `set_dash` control stroking. `clip` and `clip_even_odd` intersect the clip region with the current path and consume it. Colors are device colors (`Color::Gray(g)`, `Color::Rgb(r, g, b)`, `Color::Cmyk(c, m, y, k)`, components in `0.0..=1.0`, with `Color::BLACK` and `Color::WHITE` constants), set independently for fill (`set_fill`) and stroke (`set_stroke`). For anything the methods do not cover, `op` pushes a raw `pdfboss_core::content::Op`.

Canvases nest and carry transparency state. `group(canvas, bbox)` registers a finished sub-canvas as a Form XObject and returns a `GroupHandle`; `draw_group(handle, matrix)` paints it under a matrix, and two calls with the same handle reference one form resource. `set_fill_alpha`, `set_stroke_alpha` and `set_blend_mode` each emit a `gs` operator over a deduplicated single-key `/ExtGState` entry (`/ca`, `/CA` and `/BM` respectively); `BlendMode` covers the twelve separable modes. The resource naming contract, which `op` callers must keep consistent, is fixed: fonts are `F1`, `F2`, …, images `Im1`, …, groups `Gp1`, …, graphics states `Gs1`, …. Fonts are deduplicated document-wide, nested groups included, but a canvas registered as a group on two pages produces two Form XObjects; cross-page group sharing is deferred.

### Text

`canvas.text(text, x, y, font, size)` shows one line with its baseline origin at `(x, y)`. The faces are the fourteen standard fonts every conforming reader provides, as `Standard14` variants: `Helvetica`, `HelveticaBold`, `HelveticaOblique`, `HelveticaBoldOblique`, `TimesRoman`, `TimesBold`, `TimesItalic`, `TimesBoldItalic`, `Courier`, `CourierBold`, `CourierOblique`, `CourierBoldOblique`, `Symbol`, `ZapfDingbats`. No font program is embedded: readers carry these faces.

The twelve text faces encode as WinAnsi. A character outside the encoding is an error, never silently dropped or replaced, and the error is raised before any operator is pushed, leaving the canvas untouched. `Symbol` and `ZapfDingbats` have no encoding tables, so every character is an encoding error in those faces. The library does not wrap or lay out text: one call is one line; `Standard14::text_width(text, size)` returns a string's width from the AFM metrics (bare advance widths, no kerning) for callers doing their own layout.

Fonts are deduplicated document-wide: each distinct face gets one font object, in first-use order, no matter how many pages use it.

### Images

`ImageData` imports or wraps pixels; `add_image` registers it on a canvas and `draw_image(handle, x, y, width, height)` paints it into an axis-aligned box:

- `ImageData::png(&bytes)`: decodes a PNG; truecolor and grayscale (16-bit reduced to 8), palette expanded, alpha split into a soft mask.
- `ImageData::jpeg(&bytes)`: baseline or progressive JPEG by passthrough. The original bytes are embedded as `/DCTDecode`, dimensions sniffed from the SOF marker. Grayscale and three-component images only.
- `ImageData::rgb8(w, h, data)`, `gray8(w, h, data)`: 8-bit rasters, `data` length checked against the dimensions.
- `ImageData::mono(w, h, data)`: 1-bit rasters, rows packed MSB-first and byte-padded; a set bit is black.
- `ImageData::decode(&bytes)`: dispatches on content rather than file extension. A PNG signature goes to `png`, a JPEG SOI marker to `jpeg`, anything else is an error.

Images are embedded per page with no cross-page deduplication: the same raster drawn on two pages is stored twice.

## A complete page

One page with shapes, text in two faces, and a generated raster:

```rust,no_run
use pdfboss_core::Point;
use pdfboss_write::{Color, Date, ImageData, Metadata, Page, PageSize, Pdf, Standard14};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Page::new(PageSize::A4);
    let canvas = &mut page.canvas;

    // A stroked frame with a dash pattern.
    canvas.set_stroke(Color::Gray(0.3));
    canvas.set_line_width(1.5);
    canvas.set_dash(&[6.0, 3.0], 0.0);
    canvas.rect(36.0, 36.0, 523.28, 769.89);
    canvas.stroke();
    canvas.set_dash(&[], 0.0);

    // Filled shapes: a rectangle, a circle, a triangle.
    canvas.set_fill(Color::Rgb(0.85, 0.2, 0.2));
    canvas.rect(72.0, 600.0, 120.0, 80.0);
    canvas.fill();
    canvas.set_fill(Color::Rgb(0.2, 0.5, 0.85));
    canvas.circle(280.0, 640.0, 45.0);
    canvas.fill();
    canvas.set_fill(Color::Cmyk(0.6, 0.0, 0.9, 0.1));
    canvas.polygon(&[
        Point::new(380.0, 600.0),
        Point::new(500.0, 600.0),
        Point::new(440.0, 690.0),
    ]);
    canvas.fill();

    // Text in two of the fourteen standard faces.
    canvas.set_fill(Color::BLACK);
    canvas.text("Quarterly report", 72.0, 540.0, Standard14::HelveticaBold, 28.0)?;
    canvas.text(
        "Generated with pdfboss-write.",
        72.0,
        510.0,
        Standard14::TimesRoman,
        12.0,
    )?;

    // A generated raster, embedded and drawn at 200 x 100 pt.
    let mut pixels = Vec::with_capacity(64 * 32 * 3);
    for y in 0..32u32 {
        for x in 0..64u32 {
            pixels.extend([(x * 4) as u8, (y * 8) as u8, 128]);
        }
    }
    let gradient = ImageData::rgb8(64, 32, pixels)?;
    let handle = canvas.add_image(gradient);
    canvas.draw_image(handle, 72.0, 380.0, 200.0, 100.0);

    let pdf = Pdf {
        metadata: Some(Metadata {
            title: Some("Quarterly report".into()),
            author: Some("pdfboss".into()),
            creation_date: Some(Date {
                year: 2026,
                month: 8,
                day: 27,
                hour: 12,
                minute: 0,
                second: 0,
                utc_offset_minutes: 0,
            }),
            ..Metadata::default()
        }),
        pages: vec![page],
        ..Pdf::default()
    };
    pdf.save("report.pdf")?;
    Ok(())
}
```

To embed an existing file instead of a generated raster: `ImageData::png(&std::fs::read("photo.png")?)?`.

## Composing pages

`Page::content` holds `Content` values, an element vocabulary one step above raw canvas operators: `Text`, `Image`, `Link` and `Paragraph`, each convertible with `Content::from`, plus `Content::Custom(Box<dyn Draw>)` via `Content::custom` for anything implementing `Draw` (`fn draw(&self, canvas: &mut Canvas) -> Result<()>`, plus `Send`). Elements lower onto the page's canvas at serialization time, in order, after any operators already painted there directly, so elements paint over manual canvas work, never under it. A `Link` element is the exception: it lowers into the page's `links` vector instead of painting.

`Paragraph { text, rect, font, size, leading, align }` wraps text into its rect. `\n` forces a line break, other whitespace runs between words collapse to one space, and a blank source line keeps its vertical advance. `leading` defaults to `1.2 * size`; `align` is `ParagraphAlign::Left`, `Center`, `Right` or `Justify`, and justification stretches word spacing on every line except the last visible one. A paragraph that does not fit is an error naming how many lines fit and how many were needed, and an unencodable character errors exactly as in `canvas.text`.

`Image { data, at, width, height }` sizes itself from what is given: with both dimensions `None` it paints at the natural pixel size at 72 dpi, one given dimension scales the other by aspect, and both given paint the exact box.

```rust,no_run
use pdfboss_core::Point;
use pdfboss_write::{Content, Link, LinkTarget, Page, PageSize, Paragraph, Pdf, Standard14, Text};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Page::new(PageSize::A4);
    page.content.push(Content::from(Text {
        value: "Q3 Report".into(),
        at: Point::new(72.0, 770.0),
        font: Standard14::HelveticaBold,
        size: 28.0,
        ..Text::default()
    }));
    page.content.push(Content::from(Paragraph {
        text: "Prepared for the board. Revenue, costs and outlook for the \
               quarter, wrapped and aligned without manual line breaks."
            .into(),
        rect: [72.0, 600.0, 523.0, 740.0],
        ..Paragraph::default()
    }));
    page.content.push(Content::from(Link {
        rect: [72.0, 60.0, 200.0, 80.0],
        target: LinkTarget::Uri("https://example.com/q3".into()),
    }));
    let pdf = Pdf {
        pages: vec![page],
        ..Pdf::default()
    };
    pdf.save("q3.pdf")?;
    Ok(())
}
```

## Document slots

Beside `pages`, four `Pdf` fields carry document-level structure, each plain data and each optional.

`Outline { bookmarks }` is the viewer's bookmark panel: an ordered forest of `Bookmark { title, page, children }` nodes, `Bookmark::new(title, page)` building a leaf. Each bookmark targets a 0-based page index with an explicit `/XYZ null null null` destination, keeping the viewer's current position and zoom.

`Attachment { name, data, mime, modified, description }` embeds a file via the catalog's `/Names /EmbeddedFiles` name tree. `name` becomes the filespec's `/F` and `/UF` and the name-tree key; `data` is stored as the embedded-file stream, compressed per `WriteOptions::compress`; `mime` is the stream's `/Subtype`, defaulting to `application/octet-stream`; `modified` and `description` write `/Params /ModDate` and `/Desc` only when given. Attachments are reordered bytewise by name at emission, since the name tree's keys must be sorted, so the order given is not preserved; a duplicate name is an error.

`page_labels` holds `PageLabel { first_page, style, prefix, start_at }` ranges controlling how viewers display page numbers. A range takes effect from `first_page` (0-based) until the next range or the document's end. `LabelStyle` is `Decimal`, `RomanUpper`, `RomanLower`, `LettersUpper` or `LettersLower`, written as `/S` `D`, `R`, `r`, `A` or `a`; `prefix` prepends text to every number in the range, and `/St` is written only when `start_at` is not 1. Ranges are reordered by `first_page`, and a non-empty set must include a range starting at page 0.

`Viewer { layout, mode, open_to }` writes the catalog's opening preferences: `/PageLayout` from `PageLayout` (`SinglePage`, `OneColumn`, `TwoColumnLeft`, `TwoColumnRight`, `TwoPageLeft`, `TwoPageRight`), `/PageMode` from `PageMode` (`UseNone`, `UseOutlines`, `UseThumbs`, `FullScreen`), and `/OpenAction` opening the document at a page, keeping position and zoom.

```rust,no_run
use pdfboss_write::{
    Attachment, Bookmark, LabelStyle, Outline, Page, PageLabel, PageMode, PageSize, Pdf,
    Standard14, Viewer,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut cover = Page::new(PageSize::A4);
    cover
        .canvas
        .text("Cover", 72.0, 770.0, Standard14::HelveticaBold, 24.0)?;
    let mut body = Page::new(PageSize::A4);
    body.canvas
        .text("Body", 72.0, 770.0, Standard14::Helvetica, 12.0)?;
    let pdf = Pdf {
        pages: vec![cover, body],
        outline: Some(Outline {
            bookmarks: vec![Bookmark::new("Cover", 0), Bookmark::new("Body", 1)],
        }),
        attachments: vec![Attachment {
            name: "raw-numbers.csv".into(),
            data: b"a,b,c\n1,2,3\n".to_vec(),
            mime: Some("text/csv".into()),
            modified: None,
            description: Some("Source data".into()),
        }],
        page_labels: vec![PageLabel {
            first_page: 0,
            style: Some(LabelStyle::RomanLower),
            prefix: None,
            start_at: 1,
        }],
        viewer: Some(Viewer {
            mode: Some(PageMode::UseOutlines),
            ..Viewer::default()
        }),
        ..Pdf::default()
    };
    pdf.save("book.pdf")?;
    Ok(())
}
```

## Python: pdfboss.write

`pdfboss.write` exposes the same vocabulary as frozen values joined with `|`. A `Page(size="a4", landscape=False)` composes `Text`, `Image`, `Link` and `Paragraph` elements; a `Pdf()` composes pages, `Attachment` and `PageLabel` values (each appended, in order) and the singleton `Metadata`, `Outline` and `Viewer` slots, where a second raises `TypeError`. Every `|` returns a new value and leaves the receiver unchanged, and copies are cheap handle clones: nothing is built until `save(path)` or `to_bytes()`, which lower the composition once and release the GIL to serialize. `to_bytes` may be called repeatedly.

```python
from pdfboss.write import (
    Bookmark,
    Link,
    Metadata,
    Outline,
    Page,
    Paragraph,
    Pdf,
    Standard14,
    Text,
)

cover = (
    Page(size="a4")
    | Text("Q3 Report", at=(72, 770), font=Standard14.HELVETICA_BOLD, size=28)
    | Paragraph("Prepared for the board.", rect=(72, 700, 500, 740))
    | Link(rect=(72, 60, 200, 80), url="https://example.com/q3")
)
pdf = Pdf() | Metadata(title="Q3 Report") | cover | Outline(Bookmark("Cover", 0))
pdf.save("q3.pdf")
```

Constructors mirror the Rust fields with Python spellings: `Standard14` members are SCREAMING_SNAKE (`Standard14.HELVETICA_BOLD`), string enums are kebab-case (`align="justify"`, `style="roman-lower"`, `layout="single-page"`, `mode="use-outlines"`), `Text` takes an optional `(r, g, b)` color tuple, `Image` takes a path string or raw bytes (read and decoded only at `save`/`to_bytes` time), `Link` takes exactly one of `url` or `page`, and `Bookmark(title, page, children=(...))` nests by construction rather than `|`. The Python write surface stays clock-free, so `Metadata` and `Attachment` carry no date parameters. The full inventory is in the [Python API reference](../reference/python.md#the-write-submodule).

### The draw protocol

Any object with a callable `draw` attribute composes onto a `Page` like an element. During `save`/`to_bytes` the page's in-progress canvas is handed to `draw(canvas)` as a `Canvas` value with twelve methods: `text`, `line`, `rect`, `move_to`, `line_to`, `curve_to`, `close`, `stroke`, `fill`, `set_fill`, `set_stroke` and `set_line_width`. Painting lands in content order, exactly where the object sits in the `|` chain.

```python
from pdfboss.write import Page, Pdf, Text


class Letterhead:
    def draw(self, canvas):
        canvas.line(72, 806, 523, 806, width=0.5)
        canvas.text("ACME GmbH", at=(72, 812), size=8)


page = Page(size="a4") | Letterhead() | Text("Body copy", at=(72, 700))
data = (Pdf() | page).to_bytes()
```

The canvas is only usable inside the call: any method raises `PdfError` once `draw` has returned. An exception raised inside `draw` propagates from `save`/`to_bytes` exactly as the Python code raised it. The protocol is structural: the stub declares a `Draw` protocol type for checkers, but there is no runtime class to import or inherit.

## Metadata and dates

`Metadata` fills the document information dictionary: `title`, `author`, `subject`, `keywords`, `creator`, `producer`, `creation_date`, `modification_date`. Every field is an `Option`, and an all-`None` value writes no `/Info` dictionary at all. Dates are explicit `Date { year, month, day, hour, minute, second, utc_offset_minutes }` values; the writer never reads a clock, so dates appear in output only when supplied.

Any `Some` metadata, all-`None` included, also writes an XMP metadata stream wired into the catalog as `/Metadata`, built from the same value so the two never drift: `title` becomes `dc:title`, `author` `dc:creator`, `subject` `dc:description`, `keywords` `pdf:Keywords`, `producer` `pdf:Producer`, `creator` `xmp:CreatorTool`, and the dates `xmp:CreateDate`/`xmp:ModifyDate` in ISO-8601. The packet carries no `xmpMM:InstanceID`, no `xmpMM:DocumentID` and no generated timestamps. Nothing in the crate reads clocks or randomness, and the file identifier derives from a hash of the emitted body, so the same input produces byte-identical output.

## Writing the file

Four paths produce the same bytes:

- `pdf.save(path)`: serialize and write to a file.
- `pdf.to_bytes()`: the whole file as one `Vec<u8>`.
- `pdf.write_into(impl std::io::Write)`: the same bytes streamed in bounded chunks. An error can leave a prefix of the file already written, and no flush is performed. Flush a buffered writer yourself.
- `pdf.write_into_with(sink).await`: the asynchronous twin over any `AsyncByteSink`; it hands the sink back unflushed. `Vec<u8>` is a sink, `Immediate` presents any `std::io::Write` as one, and `pdfboss_aio::TokioSink` (behind that crate's `write` feature) presents any `tokio::io::AsyncWrite`.

```rust,no_run
use pdfboss_write::{Page, PageSize, Pdf, Standard14};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("Asynchronous emission", 72.0, 770.0, Standard14::Helvetica, 14.0)?;
    let pdf = Pdf {
        pages: vec![page],
        ..Pdf::default()
    };
    let bytes = pdf.write_into_with(Vec::new()).await?;
    tokio::fs::write("async.pdf", &bytes).await?;
    Ok(())
}
```

To stream into a tokio writer directly, wrap it in `pdfboss_aio::TokioSink`
(the `write` feature of `pdfboss-aio`). The one line is
`pdf.write_into_with(TokioSink(writer)).await?`; the writer comes back out of
the returned sink's `.0` field, unflushed, so flush it yourself:

```rust,no_run
use pdfboss_aio::TokioSink;
use pdfboss_write::{Page, PageSize, Pdf, Standard14};
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("Streamed through tokio", 72.0, 770.0, Standard14::Helvetica, 14.0)?;
    let pdf = Pdf {
        pages: vec![page],
        ..Pdf::default()
    };
    let file = tokio::fs::File::create("streamed.pdf").await?;
    let mut sink = pdf.write_into_with(TokioSink(file)).await?;
    sink.0.flush().await?;
    Ok(())
}
```

`Pdf::options` is a `WriteOptions` controlling file emission. `xref` picks the cross-reference flavor: `XrefStyle::Stream` (the default) emits a compact PDF 1.5+ cross-reference stream, `XrefStyle::Table` a classic `xref` table with a `trailer` dictionary readable by PDF 1.0-era consumers. `compress` Flate-compresses stream data that carries no filter of its own (JPEG passthrough keeps its `/DCTDecode` and is never recompressed). `object_streams` packs non-stream objects into object streams, effective only with `XrefStyle::Stream`. `version` is the header version. The defaults are `Stream`, compressed, object streams on, version 1.7. For maximum compatibility:

```rust,no_run
use std::fs::File;
use std::io::{BufWriter, Write};

use pdfboss_write::{Page, PageSize, Pdf, Standard14, WriteOptions, XrefStyle};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Page::new(PageSize::Letter);
    page.canvas
        .text("Classic xref table", 72.0, 700.0, Standard14::Courier, 12.0)?;
    let pdf = Pdf {
        pages: vec![page],
        options: WriteOptions {
            xref: XrefStyle::Table,
            compress: false,
            object_streams: false,
            version: (1, 4),
        },
        ..Pdf::default()
    };
    let mut out = BufWriter::new(File::create("classic.pdf")?);
    pdf.write_into(&mut out)?;
    out.flush()?;
    Ok(())
}
```

## CLI

`pdfboss create` covers the common cases without writing Rust, plus [`create md`](./md-to-pdf.md#cli), which composes a Markdown file with a CSS theme and has its own chapter. Blank pages, with `--pages`, `--size` (`a3`, `a4`, `a5`, `letter`, `legal`) and `--landscape`:

```bash
pdfboss create blank --out blank.pdf --pages 3 --size a5 --landscape
```

A UTF-8 text file, word-wrapped into pages. `--font` takes any of the fourteen standard faces, plus `--font-size` and `--margin` in points:

```bash
pdfboss create text notes.txt --out notes.pdf --font times-roman --font-size 12
```

One page per input image (PNG or JPEG, detected by content); without `--size`, each page matches its image at 72 dpi:

```bash
pdfboss create images photo.png --out photos.pdf
```

A TOML manifest describes metadata and whole pages declaratively, mapping `[meta]`, `[[page]]`, `[[page.text]]`, `[[page.paragraph]]`, `[[page.image]]` and `[[page.link]]` tables onto the element vocabulary above; the schema is in the [CLI reference](../reference/cli.md#create-manifest):

```bash
pdfboss create manifest q3.toml -o q3.pdf
```

Each result can be checked immediately with `pdfboss info`, which reports the version, page count and page sizes. The full flag reference is in the [CLI reference](../reference/cli.md).

## Round trip

The writer and the reader are two halves of the same engine: generated content streams parse with `pdfboss_core::content` like any other PDF, so a created document reads back without leaving the process.

```rust,no_run
use pdfboss_write::{Page, PageSize, Pdf, Standard14};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("Round trip", 72.0, 770.0, Standard14::Helvetica, 18.0)?;
    let pdf = Pdf {
        pages: vec![page],
        ..Pdf::default()
    };
    let bytes = pdf.to_bytes()?;

    let doc = pdfboss_core::Document::load(bytes)?;
    let first = doc.page(0)?;
    let text = pdfboss_output::extract_text(&doc, &first)?;
    println!("{text}");
    Ok(())
}
```

This prints `Round trip`. The same holds across tools: `pdfboss info` reads back the metadata, [`pdfboss text`](./text.md) extracts the drawn strings, and [`pdfboss render`](./rendering.md) rasterizes the page. One render detail: the standard fourteen faces carry no embedded font program, and rendering paints embedded programs by default. Pass `--fonts full` to substitute bundled faces and see the text in the raster.

## Watermarking an existing file

`watermark` draws the first page of one document over every page of another. It does not rewrite the base file: the result is the base's bytes followed by an incremental update (ISO 32000-1 §7.5.6) holding the overlay page as a form XObject, its resources copied into the base's object space, and one replacement dictionary per page whose content is wrapped in `q … Q` before the form is drawn. The output therefore grows by the overlay page's size, keeps the base's cross-reference style, and takes no longer than parsing the two files. An encrypted base is refused.

```rust,no_run
use pdfboss_core::Document;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = Document::open("report.pdf")?;
    let overlay = Document::open("draft-stamp.pdf")?;
    let bytes = pdfboss_write::watermark(&base, &overlay)?;
    std::fs::write("report-stamped.pdf", bytes)?;
    Ok(())
}
```

From Python, `pdfboss.write.watermark(data, overlay)` takes and returns bytes:

```python
import pdfboss

stamped = pdfboss.write.watermark(open("report.pdf", "rb").read(), open("draft-stamp.pdf", "rb").read())
open("report-stamped.pdf", "wb").write(stamped)
```
