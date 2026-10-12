#!/usr/bin/env python3
"""Measure the per-item cost of async element and span iteration.

`AsyncDocument.elements` and `AsyncDocument.spans` hand items to Python one
`await` at a time, while `Document.elements` and `Document.spans` hand them
over with a plain call. This script walks the same synthetic document both
ways and reports microseconds per item, next to a baseline of the same number
of bare `await asyncio.sleep(0)` calls, so the cost of the async handoff
itself is visible apart from the parsing work both walks share.

The document is generated in-process with `pdfboss.write`: every page carries
one line of text (a font and its spans), one small image and a grid of link
annotations, so a few hundred pages yield tens of thousands of logical
elements. No external files are read.

Usage:
    python benchmarks/bench_async_elements.py [--pages N] [--links K]
        [--repeat R] [--json]
"""

from __future__ import annotations

import argparse
import asyncio
import json
import struct
import sys
import time
import zlib
from collections.abc import Awaitable
from collections.abc import Callable
from dataclasses import asdict
from dataclasses import dataclass

import pdfboss
from pdfboss import write


@dataclass(frozen=True)
class Row:
    walk: str
    items: int
    seconds: float

    @property
    def micros_per_item(self) -> float:
        if not self.items:
            return 0.0
        return self.seconds / self.items * 1e6


def png_chunk(kind: bytes, payload: bytes) -> bytes:
    body = kind + payload
    return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))


def gray_png(side: int) -> bytes:
    header = struct.pack(">IIBBBBB", side, side, 8, 0, 0, 0, 0)
    rows = b"".join(b"\x00" + bytes([128]) * side for _ in range(side))
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", header)
        + png_chunk(b"IDAT", zlib.compress(rows))
        + png_chunk(b"IEND", b"")
    )


def link_grid(links: int) -> list[write.Link]:
    columns = 10
    return [
        write.Link(
            (
                40 + (index % columns) * 50,
                40 + (index // columns) * 20,
                80 + (index % columns) * 50,
                55 + (index // columns) * 20,
            ),
            url=f"https://example.com/{index}",
        )
        for index in range(links)
    ]


def build_pdf(pages: int, links: int) -> bytes:
    image = gray_png(16)
    pdf = write.Pdf()
    for number in range(pages):
        page = write.Page("a4") | write.Text(f"Page {number + 1}", at=(40, 800))
        page = page | write.Image(image, at=(480, 780), width=40, height=40)
        for link in link_grid(links):
            page = page | link
        pdf = pdf | page
    return pdf.to_bytes()


def best_of(repeat: int, walk: Callable[[], int]) -> tuple[int, float]:
    timings = []
    items = 0
    for _ in range(repeat):
        start = time.perf_counter()
        items = walk()
        timings.append(time.perf_counter() - start)
    return items, min(timings)


async def best_of_async(repeat: int, walk: Callable[[], Awaitable[int]]) -> tuple[int, float]:
    timings = []
    items = 0
    for _ in range(repeat):
        start = time.perf_counter()
        items = await walk()
        timings.append(time.perf_counter() - start)
    return items, min(timings)


def sync_elements(document: pdfboss.Document, pages: list[int]) -> int:
    return sum(1 for _ in document.elements(physical=False, pages=pages))


async def async_elements(document: pdfboss.AsyncDocument, pages: list[int]) -> int:
    count = 0
    async for _ in document.elements(physical=False, pages=pages):
        count += 1
    return count


def sync_spans(document: pdfboss.Document, pages: list[int]) -> int:
    return sum(1 for _ in document.spans(pages))


async def async_spans(document: pdfboss.AsyncDocument, pages: list[int]) -> int:
    count = 0
    async for _ in document.spans(pages):
        count += 1
    return count


async def bare_awaits(count: int) -> int:
    for _ in range(count):
        await asyncio.sleep(0)
    return count


async def measure(data: bytes, repeat: int) -> list[Row]:
    document = pdfboss.Document(data=data)
    async_document = await pdfboss.AsyncDocument.from_bytes(data)
    pages = list(range(document.page_count))

    sync_count, sync_seconds = best_of(repeat, lambda: sync_elements(document, pages))
    async_count, async_seconds = await best_of_async(
        repeat, lambda: async_elements(async_document, pages)
    )
    baseline_count, baseline_seconds = await best_of_async(
        repeat, lambda: bare_awaits(async_count)
    )
    sync_span_count, sync_span_seconds = best_of(repeat, lambda: sync_spans(document, pages))
    async_span_count, async_span_seconds = await best_of_async(
        repeat, lambda: async_spans(async_document, pages)
    )
    return [
        Row("elements, sync for", sync_count, sync_seconds),
        Row("elements, async for", async_count, async_seconds),
        Row("asyncio.sleep(0), same count", baseline_count, baseline_seconds),
        Row("spans, sync for", sync_span_count, sync_span_seconds),
        Row("spans, async for", async_span_count, async_span_seconds),
    ]


def print_table(rows: list[Row], pages: int, links: int, size: int) -> None:
    print(f"pdfboss {pdfboss.__version__}: {pages} pages, {links} links/page, {size / 1e6:.1f} MB")
    print(f"  {'walk':30} {'items':>8} {'seconds':>9} {'µs/item':>9}")
    for row in rows:
        print(f"  {row.walk:30} {row.items:8} {row.seconds:9.3f} {row.micros_per_item:9.1f}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--pages", type=int, default=500, help="pages in the generated PDF")
    parser.add_argument("--links", type=int, default=60, help="link annotations per page")
    parser.add_argument("--repeat", type=int, default=3, help="repeats; the best is kept")
    parser.add_argument("--json", action="store_true", help="print JSON instead of a table")
    args = parser.parse_args()
    if args.pages < 1 or args.links < 0 or args.repeat < 1:
        parser.error("--pages and --repeat must be at least 1, --links at least 0")

    data = build_pdf(args.pages, args.links)
    rows = asyncio.run(measure(data, args.repeat))
    if not args.json:
        print_table(rows, args.pages, args.links, len(data))
        sys.exit(0)
    report = {
        "pdfboss": pdfboss.__version__,
        "pages": args.pages,
        "links_per_page": args.links,
        "bytes": len(data),
        "repeat": args.repeat,
        "walks": [asdict(row) | {"micros_per_item": row.micros_per_item} for row in rows],
    }
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
