"""Composing new PDFs: pages, elements and document slots joined with |, and
watermarking existing ones in place."""

import pdfboss._pdfboss  # noqa: F401  registers pdfboss._pdfboss.write in sys.modules
from pdfboss._pdfboss.write import (
    Attachment,
    Bookmark,
    Canvas,
    Image,
    Link,
    Metadata,
    Outline,
    Page,
    PageLabel,
    Paragraph,
    Pdf,
    Standard14,
    Text,
    Update,
    Viewer,
    watermark,
)

__all__ = [
    "Attachment",
    "Bookmark",
    "Canvas",
    "Image",
    "Link",
    "Metadata",
    "Outline",
    "Page",
    "PageLabel",
    "Paragraph",
    "Pdf",
    "Standard14",
    "Text",
    "Update",
    "Viewer",
    "watermark",
]
