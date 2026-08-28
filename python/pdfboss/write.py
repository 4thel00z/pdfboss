"""PDF document creation: pages composed with `|`, saved once with
`Pdf.save`/`Pdf.to_bytes`.
"""

import pdfboss._pdfboss  # noqa: F401  registers pdfboss._pdfboss.write in sys.modules
from pdfboss._pdfboss.write import Image, Link, Metadata, Page, Pdf, Standard14, Text

__all__ = [
    "Image",
    "Link",
    "Metadata",
    "Page",
    "Pdf",
    "Standard14",
    "Text",
]
