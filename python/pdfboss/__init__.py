"""PDF parsing, text extraction and rendering in pure Rust."""

from enum import StrEnum

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



class ReadingOrder(StrEnum):
    """The order a page's text is read in. Every extraction method takes one
    as its ``reading_order`` keyword, as this enum or as its string value."""

    CONTENT = "content"
    """The content stream's order, corrected by geometry (the default)."""

    STRUCTURE_TREE = "structure-tree"
    """The structure tree's order on tagged pages, content order elsewhere."""

    GEOMETRIC = "geometric"
    """Position alone: lines top to bottom, spans left to right."""


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
    "ReadingOrder",
    "Span",
    "SpanIter",
    "__version__",
    "md",
    "write",
]
