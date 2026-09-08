"""``bytes`` arguments cross into Rust as one buffer copy, never element by
element.

Extracting a ``bytes`` argument as ``Vec<u8>`` makes PyO3 walk it one item at
a time under the GIL: opening a document from memory then costs about 5 ms
per MB, and an ``await AsyncDocument.from_bytes(...)`` stalls the whole event
loop for that long before the tokio task even starts. These tests pin the
cost of a from-memory open to the by-path open, keep the event loop
responsive while the async open runs, and keep every buffer type the
functions accepted before.
"""

import asyncio
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from pdfboss import AsyncDocument, Document
from pdfboss.write import Attachment, rewrite, split

PAD_BYTES = 16 * 1024 * 1024


def padded_pdf(pad: int = PAD_BYTES) -> bytes:
    """A one-page PDF whose content stream is ``pad`` bytes of comments.

    Valid, parsed lazily, and large enough that a per-element extraction
    shows up in the clock while a single buffer copy does not.
    """
    content = b"% pad\n" * (pad // 6)
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
        4: b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream",
    }
    out = bytearray(b"%PDF-1.7\n")
    offsets = {}
    for num, body in sorted(objects.items()):
        offsets[num] = len(out)
        out += b"%d 0 obj\n%s\nendobj\n" % (num, body)
    xref_at = len(out)
    out += b"xref\n0 %d\n" % (len(objects) + 1)
    out += b"0000000000 65535 f \n"
    for num in sorted(objects):
        out += b"%010d 00000 n \n" % offsets[num]
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
        len(objects) + 1,
        xref_at,
    )
    return bytes(out)


def fastest(fn: Callable[[], object], reps: int = 3) -> float:
    """The best wall-clock time of ``reps`` calls, in seconds."""
    best = float("inf")
    for _ in range(reps):
        start = time.perf_counter()
        fn()
        best = min(best, time.perf_counter() - start)
    return best


@pytest.fixture(scope="module")
def big_pdf(tmp_path_factory: pytest.TempPathFactory) -> tuple[Path, bytes]:
    data = padded_pdf()
    path = tmp_path_factory.mktemp("bytes-arguments") / "padded.pdf"
    path.write_bytes(data)
    return path, data


def test_document_from_bytes_costs_like_opening_by_path(big_pdf: tuple[Path, bytes]) -> None:
    path, data = big_pdf

    by_path = fastest(lambda: Document(str(path)))
    from_bytes = fastest(lambda: Document(data=data))

    assert from_bytes <= 5 * by_path + 0.010, (
        f"Document(data=) took {from_bytes * 1000:.1f} ms against "
        f"{by_path * 1000:.1f} ms by path for {len(data) / 1e6:.0f} MB"
    )


@pytest.mark.asyncio
async def test_async_from_bytes_keeps_the_event_loop_responsive(
    big_pdf: tuple[Path, bytes],
) -> None:
    _, data = big_pdf
    gaps: list[float] = []
    stop = asyncio.Event()

    async def tick() -> None:
        last = time.perf_counter()
        while not stop.is_set():
            await asyncio.sleep(0.001)
            now = time.perf_counter()
            gaps.append(now - last)
            last = now

    ticker = asyncio.create_task(tick())
    await asyncio.sleep(0.01)
    doc = await AsyncDocument.from_bytes(data)
    stop.set()
    await ticker

    assert doc.page_count == 1
    assert max(gaps) < 0.050, f"event loop stalled {max(gaps) * 1000:.0f} ms during from_bytes"


def test_document_accepts_bytearray_and_memoryview(hello_pdf: Path) -> None:
    data = hello_pdf.read_bytes()

    assert Document(data=bytearray(data)).page_count == 1
    assert Document(data=memoryview(data)).page_count == 1


@pytest.mark.asyncio
async def test_async_document_accepts_bytearray(hello_pdf: Path) -> None:
    doc = await AsyncDocument.from_bytes(bytearray(hello_pdf.read_bytes()))

    assert doc.page_count == 1


def test_write_functions_accept_bytearray(three_pages_pdf: Path) -> None:
    data = bytearray(three_pages_pdf.read_bytes())

    assert Document(data=rewrite(data)).page_count == 3
    assert len(split(data, 2)) == 2
    assert Attachment("notes.txt", bytearray(b"hello")) is not None
