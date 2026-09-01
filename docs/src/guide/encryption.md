# Encrypted PDFs

pdfboss opens files encrypted with the PDF Standard security handler: RC4
(40–128-bit, `/V` 1–2, and `/V` 4 with crypt filter `V2`), AES-128 (`/V` 4,
crypt filter `AESV2`) and AES-256 (`/V` 5, crypt filter `AESV3`). Either the user password or the owner
password opens the document, and both unlock the same full content. A file
protected only by an owner password has an empty user password and opens
transparently, with no password at all.

Non-ASCII passwords are tried UTF-8 encoded and, for the legacy RC4/AES-128
revisions, Latin-1 encoded as well, covering both encodings real files use.

## Checking whether a file needs a password

`pdfboss info` never fails on an encrypted file. Whether the file needs a
password is the very question being asked:

```bash
pdfboss info locked.pdf
```

```text
version:   1.7
encrypted: true
pages:     unknown
```

`encrypted: true` means the file did not open with the password supplied (by
default, none). A file that opens (because it is unencrypted, protected only
by an owner password, or because `--password` carried the right value)
reports `encrypted: false` along with the full page and metadata listing:

```bash
pdfboss info --password hunter2 locked.pdf
```

```text
version:   1.7
encrypted: false
pages:     1
  page 1: 612 x 792 pt
```

## CLI

Every subcommand that reads a PDF (`info`, `text`, `md`,
`render`, `images`, `obj`, `tui`, `json`, `hex`, `meta` and `q`) takes
`--password`, accepted as either the user or the owner password; for
`meta` it only unlocks the base for reading, since writing an update
against an encrypted base stays refused until a later PR:

```bash
pdfboss text --password hunter2 locked.pdf
```

Apart from `info`, a command given no password (or a wrong one) for a
password-protected file prints an error and exits nonzero.

## Python

`Document` takes a `password` keyword, and raises `PdfError` when the file
needs a password it was not given (or the given one is wrong):

```python
import pdfboss

try:
    doc = pdfboss.Document("locked.pdf")
except pdfboss.PdfError:
    doc = pdfboss.Document("locked.pdf", password="hunter2")
print(doc.extract_text())
```

The same keyword exists on the `data=` form of the constructor and on all
three async constructors: `AsyncDocument.open`, `AsyncDocument.open_url` and
`AsyncDocument.from_bytes` (see
[Async and remote documents](./async.md)):

```python
import asyncio

import pdfboss

async def main() -> None:
    doc = await pdfboss.AsyncDocument.open("locked.pdf", password="hunter2")
    print(doc.page_count)

asyncio.run(main())
```

## Rust

`Document::open` (and `Document::load` for bytes in memory) handles the
empty-user-password case on its own and returns `Error::Encrypted` when a
real password is needed; `open_with_password`/`load_with_password` take one:

```rust,no_run
use pdfboss_core::{Document, Error};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = match Document::open("report.pdf") {
        Err(Error::Encrypted) => Document::open_with_password("report.pdf", "hunter2")?,
        other => other?,
    };
    println!("{} pages", doc.page_count());
    Ok(())
}
```

A wrong password also comes back as `Error::Encrypted`. The async document
mirrors the sync surface with `AsyncDocument::open_with_password`,
`open_url_with_password` and `from_bytes_with_password`:

```rust,no_run
use pdfboss_aio::AsyncDocument;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = AsyncDocument::open_with_password("report.pdf", "hunter2").await?;
    println!("{} pages", doc.page_count());
    Ok(())
}
```

Once a document is open, every operation ([text](./text.md),
[markdown](./markdown.md), [rendering](./rendering.md),
[images](./images.md)) works exactly as on an unencrypted file; decryption
happens transparently underneath. Full option listings live in the
[CLI reference](../reference/cli.md).
