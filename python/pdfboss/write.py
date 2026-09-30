"""Composing new PDFs: pages, elements and document slots joined with |,
watermarking existing ones in place, and merging, splitting, rotating,
rewriting, encrypting or decrypting existing ones."""

from collections.abc import AsyncIterable, AsyncIterator

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
    SplitParts,
    Standard14,
    Text,
    Update,
    Viewer,
    decrypt,
    encrypt,
    merge,
    rewrite,
    rotate,
    split,
    split_parts,
    watermark,
)


async def split_stream(
    stream: AsyncIterable[bytes], every: int, password: str = ""
) -> AsyncIterator[bytes]:
    """Splits the PDF arriving as ``stream`` (an upload body, an S3 object
    body, any async iterable of bytes chunks) into parts of ``every``
    pages, yielding each part's bytes as soon as it is built. The chunks
    are collected in memory first, since a PDF's cross-reference table
    sits at its end. Memory holds the file plus one part, and briefly two
    copies of the file while it is handed to the parser."""
    if every < 1:
        raise ValueError("every must be at least 1 page per part")
    data = bytearray()
    async for chunk in stream:
        data += chunk
    parts = split_parts(data, every, password)
    del data
    async for part in parts:
        yield part


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
    "SplitParts",
    "Standard14",
    "Text",
    "Update",
    "Viewer",
    "decrypt",
    "encrypt",
    "merge",
    "rewrite",
    "rotate",
    "split",
    "split_parts",
    "split_stream",
    "watermark",
]
