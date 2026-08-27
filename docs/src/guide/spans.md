# Styled spans

[Text extraction](./text.md) gives you a page as flowing plain text. Spans are
the layer underneath: each span is one positioned run of text together with
everything the file states about how it is shown — position, size, font,
weight, color, visibility. Use spans when you need to know not just *what* a
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
| `bold`, `italic` | From FontDescriptor evidence, falling back to the `/BaseFont` name. |
| `monospace`, `serif` | FontDescriptor `/Flags` FixedPitch and Serif. |
| `underline`, `strikethrough` | A drawn ruling below the baseline / across the x-height band — see the caveat below. |
| `rise` | The text rise (`Ts`) the span was shown under: positive above the baseline — a superscript/subscript signal. |
| `vertical` | Writing mode 1: the text advances downward. |
| `invisible` | Shown under render mode 3 or 7, which paint nothing. |
| `color` | Fill color as RGB in `[0, 1]`; `None` for pattern fills. |

Three of these deserve honesty up front:

- **`underline` and `strikethrough` are read from the page's geometry.** PDF
  has no underline attribute; a span is underlined when a drawn ruling sits
  just below its baseline covering most of it. A table border hugging a cell's
  text can read as an underline.
- **`invisible` is the signature of an OCR text layer.** Scanned PDFs with a
  text layer draw the page image and then show the recognized text under
  render mode 3 or 7, which paint nothing. The text extracts normally — it is
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

Finding headings — bold text larger than the document's body size — is a
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

Both have async twins — `await page.spans()` and `async for span in
doc.spans()` — described in [Async and remote documents](./async.md).

## Rust

`pdfboss_text::extract_spans` returns a `Vec<TextSpan>` carrying the same
fields as the Python `Span` (as plain struct fields: `text`, `x`, `y`,
`end_x`, `size`, `font`, `font_name`, `page`, `bbox`, `bold`, `italic`,
`monospace`, `serif`, `rise`, `vertical`, `invisible`, `color`, `underline`,
`strikethrough`):

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
each stream that could not be read — an empty span list with an empty report
really is an empty page), `extract_spans_reporting_cached` (one `FontCache`
shared across a whole-document walk, the same trick `Document.spans()` uses),
and `extract_spans_and_rulings_reporting`, which additionally returns the
page's `Ruling` segments — the drawn lines the underline and strikethrough
flags are derived from.

Spans are the input to the layout analysis behind
[Markdown output](./markdown.md); reach for that chapter when you want
headings, lists and tables inferred for you rather than deriving them from
spans yourself.
