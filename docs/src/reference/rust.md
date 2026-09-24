# Rust crate reference

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
| `pdfboss-write` | PDF creation: COS object writer, content canvas, composed elements (text, paragraph, image, link), outlines, attachments, page labels, viewer preferences, XMP metadata and document assembly | [docs.rs](https://docs.rs/pdfboss-write) |
| `pdfboss-style` | CSS-subset themes for document composition | [docs.rs](https://docs.rs/pdfboss-style) |
| `pdfboss-markdown` | CommonMark+GFM composed into themed PDFs | [docs.rs](https://docs.rs/pdfboss-markdown) |
| `pdfboss-aio` | Async, range-fetching PDF access: huge files, many documents, remote HTTP sources | [docs.rs](https://docs.rs/pdfboss-aio) |
| `pdfboss-cli` | The `pdfboss` command-line tool | [docs.rs](https://docs.rs/pdfboss-cli) |
| `pdfboss-tui` | Terminal explorer for PDF internals: element tree, object inspector, hex view, page preview and Markdown preview | [docs.rs](https://docs.rs/pdfboss-tui) |
| `pdfboss-py` | PyO3 extension module `pdfboss._pdfboss`, built with maturin | not on crates.io; ships as the [pdfboss wheel](https://pypi.org/project/pdfboss/) |

A further workspace member, `pdfboss-testkit`, is an internal PDF fixture builder for the test suite; it is not published.

## Where to start

- Reading a document: `pdfboss_core::Document` (`open`, `load`, and their `_with_password` twins), then `page`, `page_count`, `metadata`, `version`.
- Text and markdown: `pdfboss_output::{extract_text, extract_markdown}`, each taking a `ReadingOrder` (`Content`, `StructureTree`, `Geometric`); positioned styled spans via `pdfboss_text::extract_spans`; where each image is drawn, without decoding it, via `pdfboss_text::placed_images`.
- Rasterizing: `pdfboss_render::{render_page, render_page_with_options, render_page_reporting}` and `Pixmap::save_png`; embedded images via `extract_page_images`.
- Creating: `pdfboss_write::{Pdf, Page, Canvas, Content}`; see [Creating PDFs](../guide/creating.md).
- Composing Markdown: `pdfboss_markdown::to_pdf` with a `pdfboss_style::Theme`; see [Markdown to PDF](../guide/md-to-pdf.md).
- Async and HTTP sources: `pdfboss_aio::AsyncDocument` (`open`, `open_url`, `from_bytes`); see [Async and remote documents](../guide/async.md).
- Element iteration: `pdfboss_core::Document::elements(ElementOpts)`, a lazy iterator over physical and logical elements, and the async `AsyncDocument::elements`, which returns an `ElementStream`; see [Exploring PDF internals](../guide/explorer.md).
- Forms, bookmarks and attachments: `Document::{interactive_form, form_fields, outline, named_destinations, page_labels, embedded_files, embedded_file_data, viewer_preferences}`, each with a `*_with(src, trailer)` twin for an async source (`pdfboss_core::{form_fields_with, outline_with, ...}`); see [Reading forms, bookmarks and attachments](../guide/structure.md).
- Document-level structures: `pdfboss_core::{linearization_dictionary, output_intents_with, articles_with, page_beads_with}` and the page thumbnail, piece-info and presentation readers under the same naming.
- Structure tree: `pdfboss_core::StructureTree` (`load_with`, `place_with`, `place_items_with`, `content_items_with`) and `Document::content_items(&page)`, a page's marked-content sequences and tagged annotations in tree order; see [Annotations in the reading order](../guide/text.md#annotations-in-the-reading-order).
- Editing existing files: `pdfboss_write::{merge_documents, split_document, rotate_rewrite, rewrite_document, encrypt_document, decrypt_document}` for a fresh document through the `Importer`, and `pdfboss_write::{Update, rotate_pages, set_metadata_with, watermark}` for an incremental update; see [Editing PDFs](../guide/editing.md), [Assembling documents](../guide/assembling.md) and [Encrypted PDFs](../guide/encryption.md).

The guide chapters carry compiled examples for each of these; the [Quickstart](../quickstart.md) has the shortest end-to-end one.
