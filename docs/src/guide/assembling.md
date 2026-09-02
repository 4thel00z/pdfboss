# Assembling documents

`pdfboss_write::assemble` builds new documents out of existing ones: `merge_documents` gathers selected pages from several sources under a fresh page tree (ISO 32000-1 §7.7.3), `split_document` cuts one document into consecutive-page parts, `rotate_rewrite` turns selected pages by a quarter-turn multiple in a whole fresh copy, and `rewrite_document`/`rewrite_with_metadata` write a document fresh with no page change. `merge_documents`, `split_document`, `rewrite_document`/`rewrite_with_metadata`, and `rotate_rewrite` all route through [`Importer`](#the-importer), which renumbers every object reference it meets once and copies it into the output; rotate's default append mode instead stages its change through [`Update`](./editing.md), the same way `meta` does.

An encrypted input is refused everywhere, the same way [`Update`](./editing.md) refuses one: the check is for the `/Encrypt` entry itself, not for whether the password opened it, since a plain target has no encryption of its own to carry the content into.

## Merging

`merge` combines selected pages from one or more inputs into a single fresh document, keeping the pages in argument order. A source contributes either every page or a specific, reorderable selection.

```bash
pdfboss merge report.pdf:2-9 appendix.pdf -o combined.pdf
```

`report.pdf:2-9` is a 1-based CLI range; `appendix.pdf` alone takes every page. From Python, the same selection is 0-based and expressed as a list:

```python
from pdfboss.write import merge

combined = merge([(report_bytes, [1, 2, 3, 4, 5, 6, 7, 8]), appendix_bytes])
```

Only the pages themselves are imported: document-level trees carried by the inputs, such as outlines, name trees and optional content, are not part of the merged output.

Link annotations are not yet rewritten for the new page tree. When a merged or split page carries a link annotation pointing at another page of its source, that referenced page's objects ride along in the output outside the new page tree, unreferenced by any `/Kids` array (a larger file than the selection alone would need), and the link still resolves to those carried objects rather than to whichever imported page they came from.

## Splitting

`split` cuts a document into consecutive chunks of `every` pages, the last chunk carrying whatever remains.

```bash
pdfboss split report.pdf -o 'part-%d.pdf' --every 10
```

`-o` names an output pattern containing `%d`, substituted with each part's 1-based number. Python's `split(data, every)` returns the parts as a list of bytes instead of writing files:

```python
from pdfboss.write import split

parts = split(report_bytes, every=10)
```

## Rotating

`rotate` turns selected pages by 90, 180 or 270 degrees clockwise. By default it appends an incremental update, the same way `meta` does; `--rewrite` (`rewrite=True` in Python) writes the whole document fresh instead.

```bash
pdfboss rotate report.pdf -o rotated.pdf --pages 2,4-9 --by 90
```

```python
from pdfboss.write import rotate

rotated = rotate(report_bytes, 90, pages=[1, 3, 4, 5, 6, 7, 8])
```

Either mode refuses a page inlined directly into `/Kids` with no object of its own: pdfboss does not yet restructure such a page to rotate it.

## Rewriting

`rewrite` writes a document fresh through the `Writer` with no page change: streams recompressed, unreachable objects and earlier update sections left behind. `meta --rewrite` (see [Editing PDFs](./editing.md)) is the same operation with metadata merged in along the way.

```bash
pdfboss rewrite report.pdf -o report-clean.pdf
```

```python
from pdfboss.write import rewrite

clean = rewrite(report_bytes)
```

## Append or rewrite

| Command | Default | Full rewrite |
|---|---|---|
| `meta` | appends an incremental update | `--rewrite` / `rewrite=True` |
| `overlay` | appends an incremental update | `--rewrite` / `rewrite=True` |
| `rotate` | appends an incremental update | `--rewrite` / `rewrite=True` |
| `merge` | always a fresh document | no incremental mode |
| `split` | always fresh documents | no incremental mode |
| `rewrite` | always a fresh document | its only mode |

## The Importer

`Importer::new(writer, source)` opens one source document for copying into a `Writer`, refusing an encrypted source for the same reason `merge`/`rotate`/`rewrite` do. `page(index, parent)` imports one page as a self-contained object under `parent`, translating its effective resources, media box and rotation and returning the page's new reference; it is what `merge_documents` calls once per selected page. `document()` instead walks the whole reachable graph from the source catalog and returns the new root reference; `rewrite_document` and `rotate_rewrite` build on it.

```rust,no_run
use pdfboss_core::{Dict, Document, Name, Object};
use pdfboss_write::{Importer, WriteOptions, Writer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let mut writer = Writer::new(WriteOptions::default());
    let pages_ref = writer.reserve();

    let mut importer = Importer::new(&mut writer, &doc)?;
    let kids = vec![importer.page(2, pages_ref)?, importer.page(0, pages_ref)?];

    let mut tree = Dict::new();
    tree.insert(Name("Type".to_string()), Object::Name(Name("Pages".to_string())));
    tree.insert(
        Name("Kids".to_string()),
        Object::Array(kids.into_iter().map(Object::Ref).collect()),
    );
    tree.insert(Name("Count".to_string()), Object::Int(2));
    writer.fill(pages_ref, Object::Dict(tree))?;

    let mut catalog = Dict::new();
    catalog.insert(Name("Type".to_string()), Object::Name(Name("Catalog".to_string())));
    catalog.insert(Name("Pages".to_string()), Object::Ref(pages_ref));
    let root = writer.put(Object::Dict(catalog));

    std::fs::write("reordered.pdf", writer.finish(root)?)?;
    Ok(())
}
```

`substitute(r, body)` replaces the body a queued reference `r` will get once the drain reaches it, with `body` already in target space; `copy(obj)` translates a source-space object's own references into the target, the piece `substitute` callers use to build that body. `rotate_rewrite` is exactly this pattern: it calls `document()` to queue the whole graph, but first calls `copy` on each selected page's dictionary (with `/Rotate` set to its new value) and `substitute`s the result for that page's own reference, so the drain fills it with the rotated body instead of the source's original one. A substitution must be staged before the drain reaches that reference; `page` itself never risks the ordering, since it only substitutes a reference it has just queued for the first time.
