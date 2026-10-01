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
    PartBuilder,
    Pdf,
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
    watermark,
)


class SplitIterator:
    """Iterator over the parts of a split, returned by ``split_parts``.
    ``next()`` builds one part with the GIL released; ``anext()`` builds
    one on a worker thread, so the event loop keeps running. A part built
    for a cancelled ``anext()`` is handed to the next one, and a step that
    is never awaited takes nothing, so no part is lost. Awaiting a second
    ``anext()`` while one is pending raises ``RuntimeError``, so parts
    always arrive in order."""

    def __init__(self, builder: PartBuilder) -> None:
        self.builder = builder
        self.advancing = False

    def __iter__(self) -> "SplitIterator":
        return self

    def __next__(self) -> bytes:
        self.refuse_overlap()
        part = self.builder.next_part()
        if part is None:
            raise StopIteration
        return part

    def __aiter__(self) -> "SplitIterator":
        return self

    async def __anext__(self) -> bytes:
        self.refuse_overlap()
        self.advancing = True
        try:
            await self.builder.build()
            part = self.builder.take()
        finally:
            self.advancing = False
        if part is None:
            raise StopAsyncIteration
        return part

    def refuse_overlap(self) -> None:
        if self.advancing:
            raise RuntimeError("SplitIterator is already advancing: await the previous step first")


def split_parts(
    data: bytes | bytearray | memoryview, every: int, *, password: str = ""
) -> SplitIterator:
    """The same parts as ``split``, built one at a time as the iterator
    advances, so only one part is held besides the input. Works with both
    ``for`` and ``async for``. The input is parsed on the first advance,
    so an unreadable ``data``, or a wrong or missing ``password``, raises
    ``PdfError`` from the first part; ``every`` below 1 raises
    ``ValueError`` here. A ``PdfError`` from any part ends the iteration.
    A protected input opened with ``password`` yields plain, unencrypted
    parts."""
    builder = PartBuilder(every, password=password)
    builder.push(data)
    return SplitIterator(builder)


async def split_stream(
    stream: AsyncIterable[bytes], every: int, *, password: str = ""
) -> AsyncIterator[bytes]:
    """Splits the PDF arriving as ``stream`` (an upload body, an S3 object
    body, any async iterable of bytes chunks) into parts of ``every``
    pages, yielding each part's bytes as soon as it is built. Each chunk
    is copied into the input as it arrives, and the input is parsed once
    the stream ends, since a PDF's cross-reference table sits at its end;
    memory holds the file plus one part. ``every`` and ``password`` are
    checked before the first chunk is read. A password-protected input
    opened with ``password`` yields plain, unencrypted parts."""
    builder = PartBuilder(every, password=password)
    async for chunk in stream:
        builder.push(chunk)
    parts = SplitIterator(builder)
    while True:
        try:
            yield await anext(parts)
        except StopAsyncIteration:
            return


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
    "SplitIterator",
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
