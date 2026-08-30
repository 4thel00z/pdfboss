# Exploring PDF internals

A PDF is two structures at once: a physical file (header, numbered objects, cross-reference sections, trailer) and the logical document those objects encode (pages, fonts, images, annotations). pdfboss exposes both as one lazy stream of elements, reachable from Python and Rust, and from the CLI as a JSON tree, jq queries, hexdumps and an interactive terminal explorer.

## The element model

A walk yields the physical elements first, in file order, each with its byte span in the file:

| kind | What it is |
|---|---|
| `header` | The `%PDF-1.x` marker |
| `object` | One indirect object (`N G obj … endobj`); `ref` carries `(num, gen)` |
| `xref` | One cross-reference section, table or stream |
| `trailer` | The trailer dictionary |
| `startxref` | The `startxref` pointer |
| `eof` | The `%%EOF` marker |

Then the logical elements, in document order:

| kind | What it is |
|---|---|
| `page` | One page; `page` carries the 0-based index |
| `font`, `image`, `annotation` | A page's resources, under that page's index |
| `content_op` | One content-stream operator; the span is the range within the page's decoded content stream |

Parsing is lazy: nothing is located, parsed or decoded before it is yielded. Iteration salvages: an element that cannot be parsed raises for that item alone, and the walk continues past it.

## Python

`Document.elements()` returns a lazy iterator; each step releases the GIL while the next element is parsed. Keyword arguments select the layers: `physical=` and `logical=` toggle the two passes, `pages=` restricts logical elements to the 0-based pages given, and `content_ops=True` adds the (high-volume) per-page operators.

```python
import pdfboss

doc = pdfboss.Document("report.pdf")

for element in doc.elements(logical=False):
    print(element.kind, element.span, element.ref)
```

Each `Element` carries `kind`, `span` (byte range, where applicable), `ref` (the `(num, gen)` object reference, where applicable) and `page` (0-based index for logical elements). `value()` converts the element lazily to plain Python data: dicts, lists, `str`, `bytes`, numbers, `bool`, `None`; PDF names become `str`, streams become `{"dict": ..., "length": n}`, references become `{"ref": (num, gen)}`. That full conversion applies to `object` and `trailer` elements; the other kinds convert to fixed shapes:

| kind | `value()` |
|---|---|
| `header` | the version string, e.g. `"1.7"` |
| `xref` | `{"kind": "table" or "stream", "entries": int}` |
| `startxref` | the offset as `int` |
| `font` | `{"subtype": str, "base_font": str or None}` |
| `image` | `{"width": int, "height": int}` |
| `annotation` | `{"subtype": str}` |
| `content_op` | the operator rendered as a string |
| `eof`, `page` | `None` |

```python
import pdfboss

doc = pdfboss.Document("report.pdf")

for element in doc.elements(physical=False, pages=[0]):
    if element.kind != "font":
        continue
    print(element.value())
```

A `for` loop stops at the first raising element, so a walk that must survive damage drives the iterator explicitly. A per-item `PdfError` leaves the iterator usable:

```python
import pdfboss

doc = pdfboss.Document("report.pdf")

elements = doc.elements()
while True:
    try:
        element = next(elements)
    except StopIteration:
        break
    except pdfboss.PdfError as err:
        print("unreadable element:", err)
        continue
    print(element.kind)
```

`AsyncDocument.elements()` is the async twin: same arguments, same ordering, same salvage semantics, consumed with `async for` ([Async and remote documents](./async.md)).

## Rust

The same walk in Rust is `pdfboss_core::Document::elements(ElementOpts)`, an iterator of `Result<Element>`. `ElementOpts` selects the layers with the same four knobs (`physical`, `logical`, `pages`, `content_ops`); `Element` is an enum; variants carry their payload fields directly, with byte spans on the physical variants.

```rust,no_run
use pdfboss_core::{Document, Element, ElementOpts};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    for element in doc.elements(ElementOpts::default()) {
        match element {
            Ok(Element::IndirectObject { r, span, .. }) => {
                println!("{} {} obj at {}..{}", r.num, r.gen, span.start, span.end);
            }
            Ok(_) => {}
            Err(err) => eprintln!("unreadable element: {err}"),
        }
    }
    Ok(())
}
```

`pdfboss_aio::AsyncDocument::elements(ElementOpts)` returns an `ElementStream`, a `futures_core::Stream` of `Result<Element>` with the same ordering and salvage semantics. The stream owns an `Arc` clone of the document, so it is `'static` and can be spawned:

```rust,no_run
use futures_util::StreamExt;
use pdfboss_aio::AsyncDocument;
use pdfboss_core::ElementOpts;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = AsyncDocument::open("report.pdf").await?;
    let mut elements = doc.elements(ElementOpts::default());
    while let Some(element) = elements.next().await {
        match element {
            Ok(element) => println!("{element:?}"),
            Err(err) => eprintln!("unreadable element: {err}"),
        }
    }
    Ok(())
}
```

## CLI

Four subcommands plus the terminal explorer. `json`, `q`, `hex` and `tui` accept a local path or an http(s) URL (a URL is fetched in ranges when the server honors `Range`; one that doesn't costs a single full download); `obj` takes a local path. Full flag listings are in the [CLI reference](../reference/cli.md).

### json: the document as a JSON value tree

```bash
pdfboss json report.pdf > report.json
```

The tree's top-level keys are `header`, `objects` (keyed `"N G"`), `pages`, `xref`, `trailer` and `startxref`. Physical entries carry a `_span` byte range; indirect references appear as `{"_r": [num, gen]}`. `--layout` adds a top-level `layout` array: per page, the inferred blocks (headings, paragraphs, lists, tables). `--pages` restricts the logical layer, `--no-logical` skips it, `--content-ops` adds per-page operators, and `--raw`/`--decode` embed stream data as base64 (still encoded, or decoded).

### q: jq programs over the same tree

```bash
pdfboss q report.pdf '. | keys'
pdfboss q report.pdf '[.pages[].fonts[].base_font] | unique'
pdfboss q report.pdf '.objects["2 0"]'
```

The second one answers "which fonts does this document use" in one line:

```text
[
  "BAMEDE+StoneSans-Bold",
  "BAMFAO+StoneSerif-Italic",
  ...
  "Helvetica",
  "Helvetica-Bold"
]
```

`-r` prints string results raw, and `--hex` hexdumps any result that carries a `_span` instead of printing its JSON: a query language for choosing what to dump.

### hex: the bytes themselves

```bash
pdfboss hex report.pdf obj:2              # one object's bytes
pdfboss hex report.pdf trailer            # or: header, xref:0, range:0x100-0x140
pdfboss hex report.pdf --annotate         # whole file, element boundaries labeled
```

```text
000000aa  32 20 30 20 6f 62 6a 0d  3c 3c 20 0d 2f 50 72 6f  |2 0 obj.<< ./Pro|
000000ba  63 53 65 74 20 5b 20 2f  50 44 46 20 2f 54 65 78  |cSet [ /PDF /Tex|
```

Selectors: `obj:N[,G]`, `header`, `xref:N` (sections indexed in chain order, newest first), `trailer`, `range:START-END` (offsets decimal or 0x-hex); without one, the whole file. `--annotate` prints a labeled boundary line as the dump crosses each element.

### obj: one object, pretty-printed

```bash
pdfboss obj report.pdf 2
```

```text
<<
  /ColorSpace <<
    /Cs5 122 0 R
    ...
  >>
  /ExtGState <<
    /GS1 148 0 R
  >>
  /Font <<
    /F1 132 0 R
    ...
  >>
  /ProcSet [/PDF /Text /ImageB /ImageC]
  ...
>>
```

### tui: the interactive explorer

```bash
pdfboss tui report.pdf
```

The screen splits into a tree pane on the left, an inspector above a hex pane on the right, and a status bar. The tree is the element model as a lazy hierarchy, populated by background tasks as sections expand: Document → Pages (each with its Fonts, Images, Annotations and Contents) → Objects → Xref sections → Trailer. The inspector pretty-prints the selection; `d` cycles it through raw bytes, decoded bytes and disassembled content operators for streams, and `Enter` jumps through any `N G R` reference under the cursor (`Backspace` goes back). `p` swaps the inspector for a rasterized page preview and `m` for the page's Markdown. The hex pane tracks the selection's bytes.

`Tab` cycles focus, arrows or `j`/`k`/`h`/`l` move, `g`/`G` jump to top/bottom, `/` searches (with `n`/`N` for next/previous hit), `q` quits. `Ctrl+Shift+arrows` resize the panes: left/right move the tree divider, up/down the inspector/hex divider (terminals that do not transmit the Shift bit can use plain `Ctrl+arrows`). Long operations (element streaming, hex fetches, search, preview rasterization) run off the event loop, so input never blocks, including over an HTTP-backed document.

`y` opens a yank menu that copies the selection to the clipboard: `q` the `pdfboss q` expression addressing it (`.objects["12 0"]`, `.pages[0].fonts`, `.trailer`), `c` the full shell command, `x` a hexdump of its bytes, `b` the raw bytes, `v` the pretty-printed value, `o` the object reference (`12 0 R`), `Esc` cancels. Copies go to the native clipboard, falling back to the OSC 52 escape sequence (which works over SSH in terminals that support it). A selection past 1 MiB yields the equivalent `pdfboss hex` command instead of the bytes.
