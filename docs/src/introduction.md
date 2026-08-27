# Introduction

pdfboss is a PDF engine written from scratch in safe Rust against the ISO 32000
specification. It is a clean-room implementation: no C dependencies, no
bindings to another engine, and the image and color codecs a PDF needs —
JPEG 2000, JBIG2, CCITT, ICC — are its own. One core sits behind every
surface: the `pdfboss` command-line tool, an interactive terminal explorer
(`pdfboss tui`), a set of Rust library crates, and a native Python extension.

The surfaces expose the same engine at different altitudes. The CLI covers
extraction, rendering, creation and explorer subcommands over a document's
structure; the terminal explorer browses a file interactively, local or
remote. Python gets `Document` and its async twin `AsyncDocument`, with
pages, styled spans, lazy element iteration, rendering and image extraction.
The Rust crates split the core by concern, from parsing (`pdfboss-core`)
through layout analysis (`pdfboss-output`) and rasterization
(`pdfboss-render`) to creation (`pdfboss-write`).

## Leniency

Real-world PDFs are damaged: truncated downloads, editors that miscount stream
lengths, generators that write cross-reference tables pointing nowhere.
pdfboss reads them anyway:

- A broken or missing cross-reference table is reconstructed by scanning the
  file for its objects.
- A stream whose declared length is wrong is still decoded.
- Content-stream operators that will not parse are skipped, and the rest of
  the page still extracts and rasterizes.

Leniency never hides what it cost. Every dropped or approximated piece of
content lands in a report: `pdfboss render` warns on stderr, the terminal
explorer raises a notice, and the libraries return the report as a value —
`render_page_reporting` and `extract_text_reporting` in Rust,
`Page.render_reporting()` in Python. A page that came out exactly as the file
describes it carries an empty report, so silence means fidelity, not luck.

## Scope

pdfboss extracts plain text and Markdown (headings, lists and tables inferred
from page layout), yields styled text spans carrying position, font, weight,
decorations and color, rasterizes pages to PNG through its own JPEG 2000,
JBIG2, CCITT and ICC codecs, extracts embedded images at their native pixel
size, creates new PDFs (the `pdfboss-write` crate and the `pdfboss create`
subcommand — creation has no Python API), and reads documents asynchronously
over range-fetching I/O, from local files or HTTP, without ever reading the
whole file.

Encrypted files open through the standard security handler — RC4 and
AES-128/256, with either the user or the owner password; a file whose user
password is empty opens without one.

## Where to go next

[Installation](./installation.md) covers the wheel, the binary and the crates;
the [Quickstart](./quickstart.md) shows each surface doing real work. The
guide then takes one task per chapter:

- [Extracting text](./guide/text.md) — plain text, reading order, page
  selection.
- [Markdown output](./guide/markdown.md) — headings, lists and tables
  inferred from layout.
- [Styled spans](./guide/spans.md) — positioned text runs with font, size,
  weight, decorations and color.
- [Rendering pages](./guide/rendering.md) — PNG output, scale, font tiers,
  render reports.
- [Extracting images](./guide/images.md) — every image a page draws, at
  native size.
- [Creating PDFs](./guide/creating.md) — blank, text and image pages from
  the CLI and Rust.
- [Async and remote documents](./guide/async.md) — range-fetching access to
  local files and HTTP URLs.
- [Exploring PDF internals](./guide/explorer.md) — the JSON value tree, jq
  queries, hexdumps and the terminal explorer.
- [Encrypted documents](./guide/encryption.md) — passwords on every surface.

The reference section holds the [CLI reference](./reference/cli.md), the
[Python API](./reference/python.md), the [Rust crates](./reference/rust.md)
and the list of [limitations](./reference/limitations.md).

pdfboss is dual-licensed under MIT or Apache-2.0, at your option.
