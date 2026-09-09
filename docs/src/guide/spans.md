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
| `size` | Effective font size. |
| `font` | Font resource name (e.g. `"F1"`). |
| `font_name` | The font's `/BaseFont` name verbatim, subset prefix included (e.g. `"NZEVTB+Arial-BoldItalicMT"`); empty when the file names the font nowhere. |
| `page` | 0-based index of the page the span came from. |
| `bbox` | Device-space box `(x0, y0, x1, y1)`, y-up: origin to advance horizontally, the font's descent..ascent vertically. |
| `ascent`, `descent` | The box's offsets from the baseline in device units: `y + ascent` is the box top, `y + descent` (zero or negative) its bottom. See [Box and baseline](#box-and-baseline). |
| `bold`, `italic` | From FontDescriptor evidence, falling back to the `/BaseFont` name. |
| `monospace`, `serif` | FontDescriptor `/Flags` FixedPitch and Serif. |
| `underline`, `strikethrough` | A drawn ruling below the baseline / across the x-height band, or a text markup annotation over the span. See [Decorations](#decorations). |
| `highlight`, `highlight_color` | A marker-style filled rectangle behind the span, or a `/Highlight` annotation over it, and its RGB color. See [Decorations](#decorations). |
| `rise` | The text rise (`Ts`) the span was shown under: positive above the baseline, a superscript/subscript signal. |
| `vertical` | Writing mode 1: the text advances downward. |
| `invisible` | Shown under render mode 3 or 7, which paint nothing. |
| `color` | Fill color as RGB in `[0, 1]`; `None` for pattern fills. |

Two of these deserve a note up front:

- **`invisible` is the signature of an OCR text layer.** Scanned PDFs with a
  text layer draw the page image and then show the recognized text under
  render mode 3 or 7, which paint nothing. The text extracts normally: it is
  just never painted.
- **`color` is `None` for pattern fills**, which have no single color.

## Box and baseline

`y` is the baseline. The box is the em box the font declares, placed on
it: `bbox[1] == y + descent` and `bbox[3] == y + ascent`, where `ascent` is
the FontDescriptor's `/Ascent` (else `/CapHeight`, else 800 per mille) and
`descent` its `/Descent` (else -200 per mille), both scaled by the effective
size. `descent` is zero or negative, the way `/Descent` is. The box is a
font metric rather than glyph ink: a span of digits does not reach the
descent and a span of capitals does not reach the ascent. Horizontally the
box runs from the origin to the advance after the last glyph.

Vertical writing (`vertical`) takes the advance as its vertical extent, so
`ascent` and `descent` are the advance's extents above and below the origin,
and the box spans half the size to each side of the baseline.

The decoration flags below are judged in this frame: a ruling is an
underline or a strikethrough by where it crosses the box, and a highlight
band by how much of the box it covers.

## Decorations

PDF has no underline, strikethrough or highlight attribute in its text
state. The flags come from two places:

- **What the page draws.** A horizontal ruling (a stroked line or a thin
  filled bar) is an underline when it sits between 0.3 of the size below the
  baseline (or a tenth of the size under the box bottom, for a font whose
  descent reaches deeper) and 0.05 of the size above it, and a strikethrough
  when it crosses the x-height band, 0.15 to 0.6 of the size above the
  baseline. A filled rectangle about a line tall is a highlight when it is
  painted before the text, has a hue (white and gray bands are backgrounds,
  knockouts and cell fills), is lighter than the text's color, and covers at
  least half the box; `highlight_color` is its fill color, read the way
  `color` is. In every case the decoration must cover at least 60% of the
  span's width and stop within an em of the text it covers on both ends.
  That last rule is what separates decorations from table borders and
  paragraph shading, which run to the cell edge or the margin rather than to
  the text; bands that tile one shaded area (same color, same extent, one
  above the other) are judged together, so a shaded row whose text happens
  to fill it does not count when its neighbours overhang theirs. A cell
  border that ends where the cell's text ends, or shading whose every line
  is filled with text, still reads as an underline or a highlight. Coverage
  is judged per span: a mark over one word of a span that is a whole line
  covers too little of it to count.
- **Text markup annotations.** `/Highlight`, `/Underline`, `/Squiggly` and
  `/StrikeOut` annotations (ISO 32000-1 §12.5.6.10) set the flags on every
  span their `/QuadPoints` quadrilaterals cover by 60% of the width and half
  the box height; a squiggly underline counts as an underline, and
  `highlight_color` is the annotation's `/C` (or `None` without one).
  Annotations flagged Hidden or NoView, or hidden by their `/OC` entry, mark
  nothing. Authored markup is taken as such: no color, paint order or extent
  test applies to it.

Vertical writing is left unmarked: its decorations are vertical lines beside
the text, indistinguishable from column rules.

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
`end_x`, `size`, `font`, `font_name`, `page`, `bbox`, `ascent`, `descent`,
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
