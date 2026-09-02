# Editing PDFs

`pdfboss_write::Update` edits an existing document without rewriting it. It appends an incremental update (ISO 32000-1 §7.5.6): a cross-reference section holding only the changed and new objects, chained to the base document's own newest section by `/Prev`. The base's bytes never move; a conforming reader parses the newest cross-reference section first and falls back to `/Prev` only for object numbers that section does not cover, so an edit takes effect over the whole file without touching a byte of what came before it.

An encrypted base is refused outright. `Update::new` checks the trailer for an `/Encrypt` entry before reading or writing anything and returns `Error::EncryptedBase` (`cannot update an encrypted document`) if one is there, whether or not the document was opened with the correct password: the check is for the entry, not for whether decryption succeeded, because the new strings and streams an update writes would need encrypting too.

This chapter covers `meta`. `merge`, `split`, `rotate` and the whole-document `rewrite` are in [Assembling documents](./assembling.md); `overlay` and `encrypt` follow in later PRs. Existing files can already be drawn on with [`watermark`](./creating.md#watermarking-an-existing-file), built on the same incremental-append machinery.

When the catalog already names an XMP packet, `set_metadata` rebuilds it from the eight modeled `Metadata` fields alone: any other XMP property the original packet carried (a PDF/A identifier, a rights statement, edit history, a custom schema) is not carried into the new packet, though the original packet's bytes stay physically present in the base, superseded only by the appended section's newer entry for that object number.

## CLI

`pdfboss meta` sets one or more `/Info` fields and writes the result as an appended update:

```bash
pdfboss meta report.pdf -o report-titled.pdf --set title="Q3 Report" --set author="Finance"
```

`--set KEY=VALUE` repeats, one per field: `title`, `author`, `subject`, `keywords`, `creator`, `producer`. `--password` opens an encrypted input for reading; the write step still refuses it, for the reason above. `--rewrite` writes the whole document fresh instead of appending: the same [`rewrite`](./assembling.md#rewriting) operation, with the metadata merged in along the way.

## Python

```python
import pdfboss
from pdfboss.write import Update

update = Update(pdfboss.Document("report.pdf"))
update.set_metadata(title="Q3 Report", author="Finance")
update.save("report-titled.pdf")
```

`set_metadata` may be called more than once before saving: a field passed as `None` keeps whatever an earlier call staged, so calls compose. `to_bytes()` returns the same bytes without writing a file, and may be called more than once.

## Rust

```rust,no_run
use pdfboss_core::Document;
use pdfboss_write::{Metadata, Update};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let mut update = Update::new(&doc)?;
    update.set_metadata(Metadata {
        title: Some("Q3 Report".to_string()),
        author: Some("Finance".to_string()),
        ..Metadata::default()
    })?;
    update.save("report-titled.pdf")?;
    Ok(())
}
```

Calling `set_metadata` more than once on the same `Update` does not compound the way the Python binding's own staging does: each call merges its fields against the base document's own `/Info`, never against an earlier call's, so the later call wins outright and any field it leaves `None` falls back to the base's original value rather than what an earlier call set.

`append_into` writes to any `impl Write` and `bytes()` returns the bytes directly; both build the update section before a byte reaches the output, so a refused or failing update leaves nothing behind. `set`, `remove` and `reserve` on `Update` stage arbitrary objects into the same appended section for edits beyond metadata.

## Async

`pdfboss-aio`'s `write` feature carries the same append over an `AsyncDocument`, without holding the whole base in memory: `overlay_base(&doc)` reads its trailer and newest cross-reference section into an `OverlayBase`, and `append_overlay(&doc, &overlay, sink)` streams the base's bytes through any `AsyncByteSink` in 64 KiB chunks, then writes the built section in one call. `Update` itself stays synchronous; `Overlay`, `OverlayBase` and `set_metadata_with` are the pieces both sides are built from, but not `start_offset`, since `append_overlay` reads the base's own last byte to compute its pad instead of calling it.
