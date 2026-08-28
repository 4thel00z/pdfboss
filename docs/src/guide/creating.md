# Creating PDFs

The write side of pdfboss is the `pdfboss-write` Rust crate plus the `pdfboss create` CLI. This chapter is the canvas level: you place every shape, glyph run and image yourself. To compose a document from CommonMark+GFM source instead — the one creation path Python also exposes, as `pdfboss.md.to_pdf` — see [Markdown to PDF](./md-to-pdf.md); the canvas level itself has no Python API. Everything the writer emits uses the same content-stream IR the reader parses, so a created file round-trips through the rest of the toolkit.

## The document model

A document is plain data: `Pdf { metadata, pages, options }`. The fields are the composition — pages appear in the output in the order of the `Vec`, singleton slots are `Option`s, and `Default` fills everything optional. Each `Page { size, rotation, canvas, links }` carries its own painted content, plus its clickable areas: a `LinkAnnotation { rect, uri }` marks a rectangle in page user space that opens a URI, emitted as a `/Link` annotation under `/Annots`.

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

Serialization validates rather than guesses: a document with zero pages is an error, and so is a rotation that is not a multiple of 90.

## Canvas

`Canvas` is an imperative painter. Path construction (`move_to`, `line_to`, `curve_to`, `close`) is separate from painting (`fill`, `fill_even_odd`, `stroke`, `close_stroke`, `fill_stroke`, `end_path`), exactly as in PDF content streams. Convenience shapes append complete subpaths: `rect`, `circle` and `ellipse` (four Bézier arcs), and `polygon` over a slice of `pdfboss_core::Point` (fewer than three points appends nothing).

Graphics state follows the same operators: `save`/`restore` push and pop, `transform` concatenates a `pdfboss_core::Matrix` onto the CTM, and `set_line_width`, `set_line_cap`, `set_line_join`, `set_miter_limit` and `set_dash` control stroking. `clip` and `clip_even_odd` intersect the clip region with the current path and consume it. Colors are device colors — `Color::Gray(g)`, `Color::Rgb(r, g, b)`, `Color::Cmyk(c, m, y, k)`, components in `0.0..=1.0`, with `Color::BLACK` and `Color::WHITE` constants — set independently for fill (`set_fill`) and stroke (`set_stroke`). For anything the methods do not cover, `op` pushes a raw `pdfboss_core::content::Op`.

### Text

`canvas.text(text, x, y, font, size)` shows one line with its baseline origin at `(x, y)`. The faces are the fourteen standard fonts every conforming reader provides, as `Standard14` variants: `Helvetica`, `HelveticaBold`, `HelveticaOblique`, `HelveticaBoldOblique`, `TimesRoman`, `TimesBold`, `TimesItalic`, `TimesBoldItalic`, `Courier`, `CourierBold`, `CourierOblique`, `CourierBoldOblique`, `Symbol`, `ZapfDingbats`. No font program is embedded — readers carry these faces.

The twelve text faces encode as WinAnsi. A character outside the encoding is an error, never silently dropped or replaced, and the error is raised before any operator is pushed, leaving the canvas untouched. `Symbol` and `ZapfDingbats` have no encoding tables, so every character is an encoding error in those faces. The library does not wrap or lay out text — one call is one line; `Standard14::text_width(text, size)` returns a string's width from the AFM metrics (bare advance widths, no kerning) for callers doing their own layout.

Fonts are deduplicated document-wide: each distinct face gets one font object, in first-use order, no matter how many pages use it.

### Images

`ImageData` imports or wraps pixels; `add_image` registers it on a canvas and `draw_image(handle, x, y, width, height)` paints it into an axis-aligned box:

- `ImageData::png(&bytes)` — decodes a PNG: truecolor and grayscale (16-bit reduced to 8), palette expanded, alpha split into a soft mask.
- `ImageData::jpeg(&bytes)` — baseline or progressive JPEG by passthrough: the original bytes are embedded as `/DCTDecode`, dimensions sniffed from the SOF marker. Grayscale and three-component images only.
- `ImageData::rgb8(w, h, data)`, `gray8(w, h, data)` — 8-bit rasters, `data` length checked against the dimensions.
- `ImageData::mono(w, h, data)` — 1-bit rasters, rows packed MSB-first and byte-padded; a set bit is black.
- `ImageData::decode(&bytes)` — dispatches on content rather than file extension: a PNG signature goes to `png`, a JPEG SOI marker to `jpeg`, anything else is an error.

Images are embedded per page with no cross-page deduplication — the same raster drawn on two pages is stored twice.

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

## Metadata and dates

`Metadata` fills the document information dictionary: `title`, `author`, `subject`, `keywords`, `creator`, `producer`, `creation_date`, `modification_date` — every field an `Option`, and an all-`None` value writes no dictionary at all. Dates are explicit `Date { year, month, day, hour, minute, second, utc_offset_minutes }` values; the writer never reads a clock, so dates appear in output only when supplied. Nothing in the crate reads clocks or randomness — the file identifier derives from a hash of the emitted body — so the same input produces byte-identical output.

## Writing the file

Four paths produce the same bytes:

- `pdf.save(path)` — serialize and write to a file.
- `pdf.to_bytes()` — the whole file as one `Vec<u8>`.
- `pdf.write_into(impl std::io::Write)` — the same bytes streamed in bounded chunks. An error can leave a prefix of the file already written, and no flush is performed — flush a buffered writer yourself.
- `pdf.write_into_with(sink).await` — the asynchronous twin over any `AsyncByteSink`; it hands the sink back unflushed. `Vec<u8>` is a sink, `Immediate` presents any `std::io::Write` as one, and `pdfboss_aio::TokioSink` (behind that crate's `write` feature) presents any `tokio::io::AsyncWrite`.

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

`pdfboss create` covers the common cases without writing Rust — plus [`create md`](./md-to-pdf.md#cli), which composes a Markdown file with a CSS theme and has its own chapter. Blank pages, with `--pages`, `--size` (`a3`, `a4`, `a5`, `letter`, `legal`) and `--landscape`:

```bash
pdfboss create blank --out blank.pdf --pages 3 --size a5 --landscape
```

A UTF-8 text file, word-wrapped into pages — `--font` takes any of the fourteen standard faces, plus `--font-size` and `--margin` in points:

```bash
pdfboss create text notes.txt --out notes.pdf --font times-roman --font-size 12
```

One page per input image (PNG or JPEG, detected by content); without `--size`, each page matches its image at 72 dpi:

```bash
pdfboss create images photo.png --out photos.pdf
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

This prints `Round trip`. The same holds across tools: `pdfboss info` reads back the metadata, [`pdfboss text`](./text.md) extracts the drawn strings, and [`pdfboss render`](./rendering.md) rasterizes the page. One render detail: the standard fourteen faces carry no embedded font program, and rendering paints embedded programs by default — pass `--fonts full` to substitute bundled faces and see the text in the raster.
