# Reading forms, bookmarks and attachments

A PDF carries more than its pages: an interactive form with its field values, a bookmark tree, page labels such as `iv` or `A-3`, named destinations, embedded files and viewer preferences. pdfboss reads all of them from the catalog as plain data, from Python, Rust and the async surfaces. Nothing here writes: form fields are read, not filled, and signatures are read as dictionaries, not verified. The full class list is in the [Python reference](../reference/python.md#the-form-and-catalog-classes).

## Python

```python
import pdfboss

doc = pdfboss.Document("form.pdf")

for field in doc.form_fields():
    if field.field_type == "text":
        print(field.name, field.text)
    elif field.button_kind == "check-box":
        print(field.name, field.checked)
    elif field.field_type == "choice":
        print(field.name, field.selected)

for item in doc.outline():
    print(item.title, item.page, len(item.children))

labels = doc.page_labels()
print(doc.page_label(0))

for attachment in doc.embedded_files():
    with open(attachment.name, "wb") as out:
        out.write(doc.embedded_file_data(attachment))

prefs = doc.viewer_preferences()
if prefs:
    print(prefs.display_doc_title, prefs.direction)
```

`form_fields()` returns every terminal field with the entries it inherits from its parents filled in: `name` is the fully qualified name, `flags` a `FieldFlags` with one boolean per flag, `value` the field's value as plain Python data, and `widgets` the widget annotations that draw it. The typed readers `text`, `checked`, `selected`, `state` and `signature` return `None` when the field is of another type. `interactive_form()` returns the form dictionary itself (`need_appearances`, `signatures_exist`, `calculation_order`, `xfa`) or `None` when the document has no form.

`outline()` returns the bookmark tree, each item with its `title`, the 0-based `page` it points to when the destination resolves through the page tree, its `destination` (fit mode and coordinates), `open`, `color`, `bold` and `italic`, and nested `children`. `named_destinations()` maps each name to a `Destination`. `page_labels()` returns the numbering ranges, and `page_label(index)` formats one page's label. `embedded_files()` lists attachments with their names, MIME type, size and dates; `embedded_file_data(file)` decodes one. Every reader has an `AsyncDocument` coroutine of the same name.

Four more readers cover the catalog and page structures a viewer consults before showing anything. `requirements()` lists the features the document needs a reader to support, each with the handlers a reader that lacks the feature would run; pdfboss reports a JavaScript handler by name and never runs it. `legal_attestation()` returns the counts of content a certifying signature cannot vouch for, one integer attribute per count and `counts()` for the whole table, with the signer's `attestation`. On a page, `viewports()` returns the measurement viewports, each a `bbox` with a `measure` whose number-format chains (`distance`, `area`, `angle` and the rest) say how user-space lengths convert to real-world units, and `separation_info()` returns a pre-separated page's `device_colorant` with the `pages` of its separation set as 0-based indices. Both page readers have `AsyncPage` coroutine twins.

```python
for viewport in doc[0].viewports():
    if viewport.measure:
        print(viewport.name, viewport.measure.scale_ratio)

separation = doc[0].separation_info()
if separation:
    print(separation.device_colorant, separation.pages)

for requirement in doc.requirements():
    print(requirement.kind, [handler.kind for handler in requirement.handlers])

legal = doc.legal_attestation()
if legal:
    print(legal.counts()["non_embedded_fonts"], legal.attestation)
```

## Rust

```rust,no_run
use pdfboss_core::Document;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("form.pdf")?;
    for field in doc.form_fields() {
        println!("{} = {:?}", field.name, field.value);
    }
    for item in doc.outline() {
        println!("{} ({} children)", item.title, item.children.len());
    }
    if let Some(labels) = doc.page_labels() {
        println!("{} label ranges", labels.len());
    }
    for file in doc.embedded_files() {
        std::fs::write(&file.name, doc.embedded_file_data(&file)?)?;
    }
    Ok(())
}
```

The same readers exist as free functions over any async object source, named with a `_with` suffix and taking the source and the trailer dictionary: `pdfboss_core::{form_fields_with, interactive_form_with, outline_with, named_destinations_with, page_labels_with, embedded_files_with, embedded_file_data_with, viewer_preferences_with, requirements_with, legal_attestation_with, viewports_with, separation_info_with}`. `pdfboss_aio::AsyncDocument` exposes them as methods, and `Document::{requirements, legal_attestation, viewports, separation_info}` are the sync forms of the last four.

## CLI

The command line has no dedicated command for these structures, but `pdfboss q` reaches the raw catalog dictionary through the object tree: the trailer's `Root` is a reference, objects are keyed by number, and a reference prints as `{"_r": [number, generation]}`.

```bash
pdfboss q form.pdf '.trailer.value.Root'              # {"_r": [131, 0]}
pdfboss q form.pdf '.objects["131"].value | keys'     # ["AcroForm", "Names", "Pages", ...]
pdfboss q form.pdf '.objects["131"].value.AcroForm'   # {"_r": [132, 0]}
```

See [Exploring PDF internals](./explorer.md) for the tree and the query language.
