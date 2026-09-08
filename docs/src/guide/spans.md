# Styled spans

[Text extraction](./text.md) gives you a page as flowing plain text. Spans are
the layer underneath: each span is one positioned run of text together with
everything the file states about how it is shown (position, size, font,
weight, color, visibility). Use spans when you need to know not just *what* a
page says but *where* and *in what style*: finding headings, separating an OCR
layer from printed text, or feeding a layout analysis of your own.

## What a span carries

| Property | Meaning |
|---|---|
| `text` | The decoded text. |
| `x`, `y` | Device-space origin and baseline of the span. |
| `end_x` | Device-space x after the last glyph's advance. |
| `ascent`, `descent` | Font `/Ascent` (positive) and `/Descent` (typically negative) at the rendered size. `bbox.y1 ≈ y + ascent`, `bbox.y0 ≈ y + descent`. |
| `size` | Effective font size. |
| `font` | Font resource name (e.g. `"F1"`). |
| `font_name` | The font's `/BaseFont` name verbatim, subset prefix included (e.g. `"NZEVTB+Arial-BoldItalicMT"`); empty when the file names the font nowhere. |
| `page` | 0-based index of the page the span came from. |
| `bbox` | Device-space box `(x0, y0, x1, y1)`, y-up. See [Box and baseline](#box-and-baseline). |
| `bold`, `italic` | From FontDescriptor evidence, falling back to the `/BaseFont` name. |
| `monospace`, `serif` | FontDescriptor `/Flags` FixedPitch and Serif. |
| `underline`, `strikethrough`, `highlight` | Drawn decorations and text-markup annotations. See the caveat below. |
| `highlight_color` | The highlight bar's resolved RGB, when the flag came from a drawn fill. |
| `rise` | The text rise (`Ts`) the span was shown under: positive above the baseline, a superscript/subscript signal. |
| `vertical` | Writing mode 1: the text advances downward. |
| `invisible` | Shown under render mode 3 or 7, which paint nothing. |
| `color` | Fill color as RGB in `[0, 1]`; `None` for pattern fills. |

## Box and baseline

Coordinates are in **unrotated PDF user space** after every content-stream
CTM: y grows up, units are PDF points (1/72 in). They are **not** spun by
the page's `/Rotate` and are **not** flipped to image space — that is the
space `Page.render` paints in. Compare boxes to a raster only after
applying the same crop-origin translation and `/Rotate` the renderer uses.

- **`bbox` is `(x0, y0, x1, y1)`**, not `(x, y, w, h)`. Origin is
  bottom-left.
- It is the font's **`/Descent`..`/Ascent` frame** at the rendered size,
  times the run's advance — an em box, not a glyph-tight ink outline.
  Tight boxes shrink with letter case (x-height vs cap-height); this frame
  does not.
- **`y` is the baseline.** `x` is the origin of the first glyph; `end_x`
  is the origin after the last advance. `y` sits between `bbox.y0` and
  `bbox.y1`: `bbox.y0 ≈ y + descent`, `bbox.y1 ≈ y + ascent`. The
  `ascent` and `descent` fields are those two offsets, so a consumer
  never has to reverse-engineer the frame from the box.

## Decorations

PDF has no underline / strikethrough / highlight attributes. The flags
are read from the page:

- **`underline`**: a thin horizontal ruling (stroke or filled bar) whose
  centerline sits just below the baseline, covering most of the span; or
  an `/Underline` annotation whose rect/quads overlap it. Page-wide
  rules (table borders, header separators) are ignored.
- **`strikethrough`**: a thin ruling that **crosses the glyph body**
  (about 40–60% of the span box height from the bottom), or a
  `/StrikeOut` annotation. A ~1 pt filled bar through the letters is a
  strike, not an underline. A line-height highlight band is a
  highlight, not a strike.
- **`highlight`**: a filled rectangle of roughly line height, in a
  light/saturated color, sitting behind **dark** text; or a `/Highlight`
  annotation. `highlight_color` is the bar's resolved DeviceRGB when the
  evidence was a drawn fill.

`/Link` annotations are ignored. A scanned or image-only page returns an
empty span list, not an error.

Three of these deserve honesty up front:
- **`invisible` is the signature of an OCR text layer.** Scanned PDFs with a
  text layer draw the page image and then show the recognized text under
  render mode 3 or 7, which paint nothing. The text extracts normally: it is
  just never painted.
- **`color` is `None` for pattern fills**, which have no single color.

## Python

`Page.spans()` returns the page's spans in emission order. It releases the GIL
while it runs and is lenient the same way text extraction is: unreadable
content yields no spans rather than raising.

```python
import pdfboss

doc = pdfboss.Document("report.pdf")
for span in doc[0].spans():
    if not span.bold:
        continue
    print(f"{span.size:5.1f}pt  {span.font_name:30s}  {span.text!r}")
```

`Document.spans()` iterates the whole document lazily, page by page: it
buffers one page's spans at a time, extracts each page with the GIL released,
and shares one font cache across the walk, so a font used on every page loads
once. Pass `pages=[...]` (0-based) to restrict the walk, in the order given.

Finding headings (bold text larger than the document's body size) is a
document-level walk:

```python
from collections import Counter

import pdfboss

doc = pdfboss.Document("report.pdf")
sizes: Counter[int] = Counter()
bold = []
for span in doc.spans():
    sizes[round(span.size)] += len(span.text)
    if span.bold:
        bold.append(span)

body = sizes.most_common(1)[0][0]
for span in bold:
    if span.size <= body:
        continue
    print(f"page {span.page + 1}: {span.size:.0f}pt {span.text}")
```

Detecting an OCR layer is a one-liner over `invisible`:

```python
spans = doc[0].spans()
ocr = [span for span in spans if span.invisible]
print(f"{len(ocr)} of {len(spans)} spans are invisible (an OCR text layer)")
```

Both have async twins, `await page.spans()` and `async for span in
doc.spans()`, described in [Async and remote documents](./async.md).

## Rust

`pdfboss_text::extract_spans` returns a `Vec<TextSpan>` carrying the same
fields as the Python `Span` (as plain struct fields: `text`, `x`, `y`,
`end_x`, `ascent`, `descent`, `size`, `font`, `font_name`, `page`, `bbox`,
`bold`, `italic`, `monospace`, `serif`, `rise`, `vertical`, `invisible`,
`color`, `underline`, `strikethrough`, `highlight`, `highlight_color`):

```rust,no_run
use pdfboss_core::Document;
use pdfboss_text::extract_spans;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    for index in 0..doc.page_count() {
        let page = doc.page(index)?;
        for span in extract_spans(&doc, &page)? {
            if !span.bold || span.size <= 12.0 {
                continue;
            }
            println!("page {}: {:.0}pt {}", span.page + 1, span.size, span.text);
        }
    }
    Ok(())
}
```

The crate also offers `extract_spans_reporting` (an `ExtractReport` naming
each stream that could not be read: an empty span list with an empty report
really is an empty page), `extract_spans_reporting_cached` (one `FontCache`
shared across a whole-document walk, the same trick `Document.spans()` uses),
and `extract_spans_and_rulings_reporting`, which additionally returns the
page's `Ruling` segments, the drawn lines the underline and strikethrough
flags are derived from.

Spans are the input to the layout analysis behind
[Markdown output](./markdown.md); reach for that chapter when you want
headings, lists and tables inferred for you rather than deriving them from
spans yourself.
