# Extracting images from PDFs

pdfboss extracts the images a page draws (photographs, scans, logos, figures),
each decoded at its own pixel dimensions and delivered as RGBA with alpha.
Three surfaces expose it: the `pdfboss images` CLI command,
`Page.extract_images` in Python, and `pdfboss_render::extract_page_images` in
Rust. To rasterize a whole page to a single PNG instead, see
[Rendering pages](./rendering.md).

## What gets extracted

Extraction walks the page's content stream and collects an image at every
point where one is drawn:

- **Occurrence-based.** The result reflects what the page draws, not what its
  resources contain. An image drawn twice appears twice; an image XObject
  listed in the resources but never drawn does not appear at all.
- **Drawing order.** Images come back in the order the content draws them.
- **Form XObjects are followed.** An image drawn inside a form (a reused
  header graphic, a stamped figure) is reached through the form, to the same
  bounded nesting depth the renderer uses, so a form that draws itself
  terminates instead of recursing forever.
- **Inline images are included.** `BI … ID … EI` sequences embedded directly
  in the content stream extract like any image XObject.
- **Stencil masks are skipped.** An image with `/ImageMask true` paints the
  current fill color through a 1-bit stencil; it carries no pixels of its own,
  so there is nothing to extract.
- **`/SMask` becomes the alpha channel.** An image's soft mask is merged into
  the output as straight (non-premultiplied) alpha.
- **Native size.** Each image decodes at its own `/Width` × `/Height`, never
  at render resolution or at the size it occupies on the page. A 3000 × 2000
  photo scaled into a thumbnail box still comes out at 3000 × 2000.
- **Optional content is not consulted.** An image inside a hidden `/OC` group
  is still embedded in the file, so it still extracts.
- **Lenient.** Content that cannot be read or decoded contributes nothing
  rather than failing the call, matching how rendering skips what it cannot
  draw.

## Filtering small images

There is no hidden minimum-size filter: every drawn image comes back,
including one-pixel spacers and thin decorative strips. Each result carries
its native width and height, so callers apply their own threshold: the
synchronous Python and Rust examples below skip anything under 100 × 100
pixels.

## CLI

`pdfboss images` writes every image the selected pages draw as a PNG named
`page-N-image-M.png`, with both numbers 1-based and `M` counting in drawing
order within the page:

```bash
mkdir images
pdfboss images --page 6 -o images report.pdf
```

```text
wrote images/page-6-image-1.png (137 x 178 px)
wrote images/page-6-image-2.png (750 x 989 px)
wrote images/page-6-image-3.png (132 x 177 px)
wrote images/page-6-image-4.png (216 x 295 px)
wrote images/page-6-image-5.png (144 x 168 px)
wrote images/page-6-image-6.png (50 x 120 px)
wrote images/page-6-image-7.png (165 x 211 px)
wrote images/page-6-image-8.png (145 x 188 px)
wrote images/page-6-image-9.png (165 x 206 px)
extracted 9 images
```

Without `--page` every page is processed. `-o` names the output directory
(default: the current directory); it must already exist. `--png-compression`
trades encode time against file size: `none`, `fast`, `default` or `best`,
all producing the same pixels. A page whose images cannot be decoded writes
nothing for them and still exits 0.

## Python

`Page.extract_images` returns a list of `PageImage` objects, each holding the
PNG-encoded pixels as `data` plus the native `width` and `height`. Saving
every sufficiently large image in a document:

```python
from pathlib import Path

import pdfboss

doc = pdfboss.Document("report.pdf")
out = Path("images")
out.mkdir(exist_ok=True)
for number, page in enumerate(doc, start=1):
    for i, image in enumerate(page.extract_images(), start=1):
        if image.width < 100 or image.height < 100:
            continue
        target = out / f"page-{number}-image-{i}.png"
        target.write_bytes(image.data)
        print(f"{target}: {image.width} x {image.height}")
```

The drawing-order index `i` is kept even for skipped images, so file names
stay aligned with what `pdfboss images` would produce. `extract_images`
accepts the same `compression` argument as `render` (`"none"`, `"fast"`,
`"default"`, `"best"`).

`AsyncPage.extract_images` is the async twin, with the same drawing-order,
native-size and leniency semantics (see
[Async and remote documents](./async.md)):

```python
import asyncio

import pdfboss

async def main() -> None:
    doc = await pdfboss.AsyncDocument.open("report.pdf")
    page = doc.page(0)
    images = await page.extract_images()
    print(f"page 1 draws {len(images)} images")

asyncio.run(main())
```

## Rust

`pdfboss_render::extract_page_images` returns `Vec<Pixmap>`: RGBA8 pixels
with straight alpha, row-major from the top-left, with public `width`,
`height` and `data` fields. `Pixmap::save_png` writes one to disk;
`encode_png`/`encode_png_with` produce the bytes in memory. With
`pdfboss-core` and `pdfboss-render` as dependencies:

```rust,no_run
use pdfboss_core::Document;
use pdfboss_render::extract_page_images;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let page = doc.page(0)?;
    let images = extract_page_images(&doc, &page)?;
    for (i, image) in images.iter().enumerate() {
        if image.width < 100 || image.height < 100 {
            continue;
        }
        image.save_png(format!("image-{}.png", i + 1))?;
        println!("image-{}.png: {} x {}", i + 1, image.width, image.height);
    }
    Ok(())
}
```

`extract_page_images_with` is the same walk over any
`pdfboss_core::AsyncObjectSource`. `pdfboss_aio::AsyncDocument` implements
that trait and is an `Arc` handle, so cloning one to pass by value is cheap
(with `pdfboss-aio`, `pdfboss-render` and `tokio` as dependencies):

```rust,no_run
use pdfboss_aio::AsyncDocument;
use pdfboss_render::extract_page_images_with;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = AsyncDocument::open("report.pdf").await?;
    for index in 0..doc.page_count() {
        let page = doc.page(index)?;
        let images = extract_page_images_with(doc.clone(), &page).await?;
        for (i, image) in images.iter().enumerate() {
            image.save_png(format!("page-{}-image-{}.png", index + 1, i + 1))?;
        }
    }
    Ok(())
}
```

The async form works identically over a remote document opened with
`AsyncDocument::open_url`, fetching only the byte ranges the images need.
Full option listings live in the [CLI reference](../reference/cli.md).
