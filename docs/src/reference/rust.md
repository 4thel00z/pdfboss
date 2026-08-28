# Rust crates

pdfboss is a workspace of focused crates, all sharing one version. Add the ones you need with `cargo add`; each crate's API reference lives on docs.rs.

| Crate | Responsibility | Docs |
|---|---|---|
| `pdfboss-core` | PDF syntax, objects, filters, cross-references and document model (ISO 32000) | [docs.rs](https://docs.rs/pdfboss-core) |
| `pdfboss-text` | Font loading, encodings, ToUnicode CMaps and text extraction | [docs.rs](https://docs.rs/pdfboss-text) |
| `pdfboss-output` | Layout analysis and output rendering: plain text and markdown | [docs.rs](https://docs.rs/pdfboss-output) |
| `pdfboss-encoding` | Shared font encoding tables and glyph-name mappings (ISO 32000 Appendix D) | [docs.rs](https://docs.rs/pdfboss-encoding) |
| `pdfboss-jpx` | Cleanroom JPEG 2000 (`JPXDecode`) decoder (ITU-T T.800) | [docs.rs](https://docs.rs/pdfboss-jpx) |
| `pdfboss-icc` | Cleanroom ICC profile parser and colour transform (ICC.1:2010) | [docs.rs](https://docs.rs/pdfboss-icc) |
| `pdfboss-render` | Page rasterization to RGBA pixmaps and PNG, plus embedded-image extraction | [docs.rs](https://docs.rs/pdfboss-render) |
| `pdfboss-write` | PDF creation: COS object writer, content canvas, link annotations and document assembly | [docs.rs](https://docs.rs/pdfboss-write) |
| `pdfboss-style` | CSS-subset themes for document composition | [docs.rs](https://docs.rs/pdfboss-style) |
| `pdfboss-markdown` | CommonMark+GFM composed into themed PDFs | [docs.rs](https://docs.rs/pdfboss-markdown) |
| `pdfboss-aio` | Async, range-fetching PDF access: huge files, many documents, remote HTTP sources | [docs.rs](https://docs.rs/pdfboss-aio) |
| `pdfboss-cli` | The `pdfboss` command-line tool | [docs.rs](https://docs.rs/pdfboss-cli) |
| `pdfboss-tui` | Terminal explorer for PDF internals: element tree, object inspector, hex view, page preview and Markdown preview | [docs.rs](https://docs.rs/pdfboss-tui) |
| `pdfboss-py` | PyO3 extension module `pdfboss._pdfboss`, built with maturin | not on crates.io; ships as the [pdfboss wheel](https://pypi.org/project/pdfboss/) |

A further workspace member, `pdfboss-testkit`, is an internal PDF fixture builder for the test suite; it is not published.

## Where to start

- Reading a document: `pdfboss_core::Document` — `open`, `load`, and their `_with_password` twins, then `page`, `page_count`, `metadata`, `version`.
- Text and markdown: `pdfboss_output::{extract_text, extract_markdown}`; positioned styled spans via `pdfboss_text::extract_spans`.
- Rasterizing: `pdfboss_render::{render_page, render_page_with_options, render_page_reporting}` and `Pixmap::save_png`; embedded images via `extract_page_images`.
- Creating: `pdfboss_write::{Pdf, Page, Canvas}` — see [Creating PDFs](../guide/creating.md).
- Composing Markdown: `pdfboss_markdown::to_pdf` with a `pdfboss_style::Theme` — see [Markdown to PDF](../guide/md-to-pdf.md).
- Async and HTTP sources: `pdfboss_aio::AsyncDocument` — `open`, `open_url`, `from_bytes` — see [Async and remote documents](../guide/async.md).

The guide chapters carry compiled examples for each of these; the [Quickstart](../quickstart.md) has the shortest end-to-end one.
