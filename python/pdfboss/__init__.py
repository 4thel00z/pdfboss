"""PDF parsing, text extraction and rendering in pure Rust."""

from pdfboss._pdfboss import (
    AsyncDocument,
    AsyncElementIter,
    Document,
    Element,
    ElementIter,
    Page,
    PdfError,
    __version__,
)

__all__ = [
    "AsyncDocument",
    "AsyncElementIter",
    "Document",
    "Element",
    "ElementIter",
    "Page",
    "PdfError",
    "__version__",
]
