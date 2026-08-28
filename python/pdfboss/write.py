"""PDF document creation: pages composed with `|`, saved once with
`Pdf.save`/`Pdf.to_bytes`.
"""

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
    Viewer,
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
    "Viewer",
]
