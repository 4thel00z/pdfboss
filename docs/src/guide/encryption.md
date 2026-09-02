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
`--password`, accepted as either the user or the owner password. For
`meta` it only unlocks the base for reading: the incremental update it
appends by default still refuses any encrypted base outright, whether
or not the password opened it, the same refusal `rotate`'s and
`overlay`'s own default append raise, and `pdfboss_write::Update`
raises directly:

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

## Encrypting a file

`encrypt` builds a fresh AES-256, revision 6 protected copy of a document
(ISO 32000-2 §7.6.4.3). At least one of the user or owner password must
be set; an omitted owner password falls back to the user password, and
either password opens the file:

```bash
pdfboss encrypt report.pdf -o locked.pdf --user-password hunter2
```

`--allow` restricts what a reader opening under the user password may
do; the owner password always grants everything. Values: `print`,
`modify`, `copy`, `annotate`, `fill-forms`, `accessibility`, `assemble`,
`print-hires`; every permission is granted when `--allow` is omitted:

```bash
pdfboss encrypt report.pdf -o locked.pdf --user-password hunter2 --allow print,copy
```

`--password` opens an input that is itself encrypted, so an
already-protected file re-encrypts under new passwords:

```bash
pdfboss encrypt locked.pdf -o relocked.pdf --password hunter2 --user-password newpass
```

Rust:

```rust,no_run
use pdfboss_core::{Document, Permissions};
use pdfboss_write::{encrypt_document, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open("report.pdf")?;
    let permissions = Permissions { modify: false, ..Permissions::all() };
    let bytes = encrypt_document(&doc, "hunter2", "", permissions, WriteOptions::default())?;
    std::fs::write("locked.pdf", bytes)?;
    Ok(())
}
```

Python:

```python
from pdfboss.write import encrypt

locked = encrypt(report_bytes, user_password="hunter2", allow=["print", "copy"])
```

## Removing encryption

`decrypt` opens a file under its user or owner password and writes a
fresh, unencrypted copy:

```bash
pdfboss decrypt locked.pdf -o plain.pdf --password hunter2
```

```rust,no_run
use pdfboss_core::Document;
use pdfboss_write::{decrypt_document, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::open_with_password("locked.pdf", "hunter2")?;
    let bytes = decrypt_document(&doc, WriteOptions::default())?;
    std::fs::write("plain.pdf", bytes)?;
    Ok(())
}
```

```python
from pdfboss.write import decrypt

plain = decrypt(locked_bytes, password="hunter2")
```

## Limitations

pdfboss writes AES-256, revision 6 encryption only; RC4 and AES-128
files stay readable but are never produced. Each encrypted output uses
a fresh random file key, salts and initialization vectors, so
encrypting the same document twice under the same passwords never
produces identical bytes. An incremental update appended onto an
encrypted base (the default mode of `meta`, `rotate` and `overlay`, and
`pdfboss_write::Update` directly) still refuses the base outright,
whether or not it opens under a password. `encrypt` and `decrypt` are
the only commands that take a password-opened encrypted input
directly; `merge`, `split`, `rewrite` and the `--rewrite` forms of
`rotate`/`overlay` refuse every encrypted input at the CLI regardless
of password, even though the `pdfboss_write` functions behind them
would accept an already-opened one and carry its content across as
plaintext. The `/Info` metadata stream (the XMP packet), like every
other string and stream in the file, is always encrypted along with
the rest of the content.
