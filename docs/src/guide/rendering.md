# Rendering PDF pages to PNG

pdfboss rasterizes pages to RGBA pixels with anti-aliasing and encodes them as
PNG. The scale factor is the points-to-pixels ratio: at `1.0` one PDF point
becomes one pixel, so a US Letter page renders to 612 × 792 pixels; at `2.0`
to 1224 × 1584. The pixel size is `ceil(crop_w * scale) × ceil(crop_h *
scale)` after page rotation, on a white background.

This chapter is about rasterizing whole pages. To pull out the images a page
*embeds*, at their native resolution, see [Extracting images](./images.md).

## CLI

```bash
pdfboss render report.pdf --page 1
pdfboss render report.pdf --page 1 --scale 2 -o page-1@2x.png
```

The first form writes `page-1.png` (`--page` is 1-based). Further flags:

- `--fonts <FONTS>`: which fonts to paint, one of `embedded-only`,
  `all-embedded` or `full` (see the tiers below). The default resolves to
  `full` when substitute faces are available (the compiled-in OFL set or
  `--font-dir`), otherwise `all-embedded`.
- `--font-dir <FONT_DIR>`: a directory of substitute faces for `--fonts
  full` (e.g. an installed `pdfboss-fonts` package), named as listed under
  [Substitute face files](#substitute-face-files). Overrides the compiled-in
  OFL set.
- `--png-compression <PNG_COMPRESSION>`: `none`, `fast`, `default` or
  `best`.
- `--password <PASSWORD>`: for encrypted files, covered in
  [Encrypted documents](./encryption.md).

Anything the render dropped is warned on stderr.

## Font tiers

Glyph painting is staged in tiers; each tier is a strict superset of the
previous one.

- **`embedded-only`** paints only embedded TrueType outlines: the fastest
  tier, and TrueType only, not every embedded font.
- **`all-embedded`** (the default when no substitute faces are available)
  paints every embedded font program: TrueType, CFF, Type1 and Type3.
- **`full`** (the default whenever substitute faces are available)
  additionally substitutes a replacement face for non-embedded
  simple fonts: from a directory you supply (`--font-dir`, `font_dir=`), or
  from the compiled-in OFL Croscore set (Arimo/Tinos/Cousine,
  metric-compatible with Helvetica/Times/Courier). In Python, `pip install
  pdfboss[full]` installs those faces as the `pdfboss-fonts` package; with
  neither `font_dir` nor that package available, `fonts="full"` raises
  `ValueError`.

Text a tier leaves unpainted still advances (through the PDF's own `/Widths`,
or the Adobe Core-14 AFM tables for a standard-14 face), so everything painted
around it stays where the page put it. `/Symbol` and `/ZapfDingbats` have no
license-clean substitute and stay blank; see
[Limitations](../reference/limitations.md).

### Substitute face files

A substitute directory (`--font-dir`, `font_dir=`, `SubstituteSource::Dir`)
holds one file per face, looked up by these exact names:

| Family | Files |
|---|---|
| Sans (Arimo) | `Arimo[wght].ttf`, `Arimo-Italic[wght].ttf` (weight rides the variable-font `[wght]` axis) |
| Serif (Tinos) | `Tinos-Regular.ttf`, `Tinos-Bold.ttf`, `Tinos-Italic.ttf`, `Tinos-BoldItalic.ttf` |
| Mono (Cousine) | `Cousine-Regular.ttf`, `Cousine-Bold.ttf`, `Cousine-Italic.ttf`, `Cousine-BoldItalic.ttf` |

A file that is missing or unreadable means no substitution for the faces that
map to it; nothing else in the directory is read.

## Leniency and reporting

Rendering never fails because one construct in a page would not read: content
pdfboss cannot fetch, decode or parse is skipped and the rest of the page
still rasterizes. The honest consequence is that a page can come back blank
without an error. The reporting variants return what was dropped or
approximated, one line per distinct loss, for example
`"153 glyphs skipped: no glyph for code 9 in /MBIPWP+Times-Roman"` or
`"6 annotations skipped: the resource is missing"`. An empty report means the
page rasterized exactly as it describes itself.

Two things are deliberately not reported, because they are configured behavior
rather than a failure: text left unpainted by the requested font tier, and
content in optional-content layers (PDF layers) the document's default
configuration turns off. The latter is counted separately, on the report's
`hidden` counter in Rust.

## Python

`Page.render` returns PNG bytes. `scale` must be positive and finite
(`ValueError` otherwise).

```python
from pathlib import Path

import pdfboss

doc = pdfboss.Document("report.pdf")
page = doc[0]
png = page.render(scale=2.0)
Path("page-1.png").write_bytes(png)
```

`render` accepts `fonts=` (`"embedded-only"`, `"all-embedded"`, `"full"`),
`font_dir=` and `compression=` (`"none"`, `"fast"`, `"default"`, `"best"`),
and releases the GIL while it runs. `fonts=` defaults to `None`, which
resolves to `"full"` when `font_dir=` is given or the `pdfboss-fonts`
package is importable, and to `"all-embedded"` otherwise. `Page.render_reporting` renders the same
way and returns `(png, warnings)`:

```python
png, warnings = page.render_reporting()
for line in warnings:
    print(line)
```

`Document.render_pages` renders many pages fanned out across the machine's
cores: every page by default, or the 0-based `pages` given, returned in the
order given.

```python
pngs = doc.render_pages(scale=2.0)
first_two_reversed = doc.render_pages(pages=[1, 0])
```

The full signature is `render_pages(pages=None, scale=1.0, fonts=None,
font_dir=None, compression="default")`; `fonts`, `font_dir` and
`compression` mean the same as on `Page.render`, applied to every page,
and a `fonts` of `None` resolves the same way. The stub file
[`_pdfboss.pyi`](https://github.com/4thel00z/pdfboss/blob/main/python/pdfboss/_pdfboss.pyi)
documents each parameter.

All three have async twins on `AsyncPage`/`AsyncDocument`, which also render
documents opened over HTTP. See
[Async and remote documents](./async.md).

## PNG compression

The compression level trades encode time against file size; every level
produces the same pixels. `none` is fastest and largest, `fast` is very fast
with a decent ratio, `default` balances the two, and `best` produces the
smallest files, much slower. The level only touches the PNG encoder. Pick it
by whether you are writing throwaway intermediates or archiving.

## Rust

`pdfboss_render::render_page(doc, page, scale)` returns a `Pixmap`: `width`,
`height`, and `data` holding `width * height * 4` RGBA bytes (straight alpha,
row-major from the top-left). `Pixmap::save_png` writes it to a file;
`encode_png`/`encode_png_with` return the bytes, the latter taking a
`PngCompression` (`None`, `Fast`, `Balanced` or `Best`; `Balanced` is the
level the other surfaces call `default`).

`render_page_with_options` adds `RenderOptions`, a struct of four public
fields:

- `glyph_painting` selects the `GlyphPainting` tier: `EmbeddedTrueTypeOnly`,
  `AllEmbedded` or `Full`.
- `substitutes` says where the `Full` tier's replacement faces come from.
  `SubstituteSource::Builtin` is the compiled-in OFL set, present only when
  the crate is built with the `substitute-fonts` Cargo feature; without that
  feature, `Builtin` degrades to no substitution, so `Full` behaves exactly
  like `AllEmbedded`. The probe `pdfboss_render::builtin_fonts_available()`
  reports whether the compiled-in set exists.
  `SubstituteSource::Dir(path)` reads faces from a directory named as in
  [Substitute face files](#substitute-face-files). The default
  `SubstituteSource::None` substitutes nothing, so `Full` behaves like
  `AllEmbedded` until you opt in.
- `oc: Option<Arc<OcState>>` is the document's optional-content visibility.
  The synchronous entry points fill it from the document when it is `None`;
  an asynchronous caller builds it itself, from `AsyncDocument::oc_state`,
  and leaving it `None` there renders every layer. See
  [Async and remote documents](./async.md).
- `cache: Option<Arc<RenderCache>>` shares one `RenderCache` across a
  whole-document walk. It retains loaded fonts and parsed `ICCBased`
  colorspace outcomes across pages; `None` keeps every load page-local.

`render_page_reporting` returns the
`RenderReport` alongside the pixels; `report.summary()` is a one-line count
per kind, `report.warnings()` one line per distinct drop.

```rust,no_run
use pdfboss_core::Document;
use pdfboss_render::{
    render_page, render_page_reporting, GlyphPainting, PngCompression, RenderOptions,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let page = doc.page(0)?;

    let pixmap = render_page(&doc, &page, 2.0)?;
    pixmap.save_png("page-1.png")?;

    let opts = RenderOptions {
        glyph_painting: GlyphPainting::EmbeddedTrueTypeOnly,
        ..RenderOptions::default()
    };
    let (pixmap, report) = render_page_reporting(&doc, &page, 1.0, &opts)?;
    if let Some(summary) = report.summary() {
        eprintln!("{summary}");
    }
    for warning in report.warnings() {
        eprintln!("{warning}");
    }
    let png = pixmap.encode_png_with(PngCompression::Best)?;
    std::fs::write("page-1-small.png", png)?;
    Ok(())
}
```

For a whole document, pass one `RenderCache` to every page, so each font
program and each `ICCBased` profile loads once per document rather than once
per page:

```rust,no_run
use std::sync::Arc;

use pdfboss_core::{map_pages, Document};
use pdfboss_render::{render_page_with_options, RenderCache, RenderOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let opts = RenderOptions {
        cache: Some(Arc::new(RenderCache::default())),
        ..RenderOptions::default()
    };
    let outcomes = map_pages(&doc, |doc, page| {
        render_page_with_options(doc, page, 2.0, &opts)
    });
    for (index, pixmap) in outcomes.into_iter().enumerate() {
        pixmap?.save_png(format!("page-{}.png", index + 1))?;
    }
    Ok(())
}
```
