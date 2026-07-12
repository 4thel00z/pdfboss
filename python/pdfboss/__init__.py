"""PDF parsing, text extraction and rendering in pure Rust."""

from pdfboss._pdfboss import Document, Page, PdfError, __version__

__all__ = ["Document", "Page", "PdfError", "__version__"]
