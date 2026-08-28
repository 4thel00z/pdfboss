"""PDF parsing, text extraction and rendering in pure Rust."""

from pdfboss import md, write
from pdfboss._pdfboss import (
    AsyncDocument,
    AsyncElementIter,
    AsyncPage,
    AsyncSpanIter,
    Document,
    Element,
    ElementIter,
    Page,
    PageImage,
    PdfError,
    Span,
    SpanIter,
    __version__,
)

__all__ = [
    "AsyncDocument",
    "AsyncElementIter",
    "AsyncPage",
    "AsyncSpanIter",
    "Document",
    "Element",
    "ElementIter",
    "Page",
    "PageImage",
    "PdfError",
    "Span",
    "SpanIter",
    "__version__",
    "md",
    "write",
]
