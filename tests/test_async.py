"""Tests for AsyncDocument: async open, metadata and object fetch.

Runs against the committed fixture PDFs in ``tests/fixtures/``. Requires the
extension module to be built and installed (e.g. via maturin).
"""

import asyncio
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from pdfboss import AsyncDocument, Document, Element, PdfError


class TestAsyncOpen:
    @pytest.mark.asyncio
    async def test_open_by_pathlike(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(hello_pdf)
        assert doc.page_count() == 1

    @pytest.mark.asyncio
    async def test_open_by_str(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(str(hello_pdf))
        assert doc.page_count() == 1

    @pytest.mark.asyncio
    async def test_from_bytes(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.from_bytes(hello_pdf.read_bytes())
        assert doc.page_count() == 1

    @pytest.mark.asyncio
    async def test_version_matches_sync(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(hello_pdf)
        assert doc.version() == Document(str(hello_pdf)).version

    @pytest.mark.asyncio
    async def test_xref_stream_file_opens(self, xref_stream_pdf: Path) -> None:
        doc = await AsyncDocument.open(xref_stream_pdf)
        assert doc.page_count() == 1

    @pytest.mark.asyncio
    async def test_missing_file_raises_prefixed_pdf_error(
        self, tmp_path: Path
    ) -> None:
        with pytest.raises(PdfError) as exc:
            await AsyncDocument.open(tmp_path / "missing.pdf")
        assert str(exc.value).startswith(("io:", "parse:"))

    @pytest.mark.asyncio
    async def test_garbage_bytes_raise_prefixed_pdf_error(self) -> None:
        with pytest.raises(PdfError) as exc:
            await AsyncDocument.from_bytes(b"not a pdf")
        assert str(exc.value).startswith(("parse:", "io:"))


class TestAsyncDocumentQueries:
    @pytest.mark.asyncio
    async def test_metadata_matches_sync(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(hello_pdf)
        assert await doc.metadata() == Document(str(hello_pdf)).metadata

    @pytest.mark.asyncio
    async def test_get_object_fetches_the_catalog(self, hello_pdf: Path) -> None:
        trailer = next(
            e
            for e in Document(str(hello_pdf)).elements(logical=False)
            if e.kind == "trailer"
        )
        num, gen = trailer.value()["Root"]["ref"]
        doc = await AsyncDocument.open(hello_pdf)
        catalog = await doc.get_object(num, gen)
        assert catalog["Type"] == "Catalog"

    @pytest.mark.asyncio
    async def test_get_object_gen_defaults_to_zero(self, hello_pdf: Path) -> None:
        trailer = next(
            e
            for e in Document(str(hello_pdf)).elements(logical=False)
            if e.kind == "trailer"
        )
        num, gen = trailer.value()["Root"]["ref"]
        assert gen == 0
        doc = await AsyncDocument.open(hello_pdf)
        assert await doc.get_object(num) == await doc.get_object(num, gen)

    @pytest.mark.asyncio
    async def test_documents_run_concurrently(
        self, hello_pdf: Path, three_pages_pdf: Path
    ) -> None:
        docs = await asyncio.gather(
            AsyncDocument.open(hello_pdf),
            AsyncDocument.open(three_pages_pdf),
        )
        assert [d.page_count() for d in docs] == [1, 3]


def element_key(element: Element) -> tuple[object, ...]:
    """A comparable identity for an element (everything but value())."""
    return (element.kind, element.span, element.ref, element.page)


class TestAsyncElements:
    @pytest.mark.asyncio
    async def test_async_for_yields_elements(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(hello_pdf)
        kinds = []
        async for element in doc.elements():
            assert isinstance(element, Element)
            kinds.append(element.kind)
        assert kinds[0] == "header"
        assert "page" in kinds

    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        "name", ["hello.pdf", "three-pages.pdf", "shapes.pdf", "xref-stream.pdf"]
    )
    async def test_parity_with_sync_elements(
        self, fixtures_dir: Path, name: str
    ) -> None:
        path = fixtures_dir / name
        expected = [
            element_key(e) for e in Document(str(path)).elements(content_ops=True)
        ]
        doc = await AsyncDocument.open(path)
        got = []
        async for element in doc.elements(content_ops=True):
            got.append(element_key(element))
        assert got == expected

    @pytest.mark.asyncio
    async def test_values_match_sync(self, hello_pdf: Path) -> None:
        expected = [e.value() for e in Document(str(hello_pdf)).elements()]
        doc = await AsyncDocument.open(hello_pdf)
        got = []
        async for element in doc.elements():
            got.append(element.value())
        assert got == expected

    @pytest.mark.asyncio
    async def test_filters_pass_through(self, three_pages_pdf: Path) -> None:
        doc = await AsyncDocument.open(three_pages_pdf)
        pages = []
        async for element in doc.elements(physical=False, pages=[1]):
            if element.kind == "page":
                pages.append(element.page)
        assert pages == [1]

    @pytest.mark.asyncio
    async def test_event_loop_stays_responsive(self, three_pages_pdf: Path) -> None:
        doc = await AsyncDocument.open(three_pages_pdf)
        ticks = 0

        async def ticker() -> None:
            nonlocal ticks
            while True:
                ticks += 1
                await asyncio.sleep(0)

        task = asyncio.create_task(ticker())
        try:
            count = 0
            async for element in doc.elements(content_ops=True):
                count += 1
        finally:
            task.cancel()
        assert count > 0
        assert ticks > 0


class RangeRequestHandler(BaseHTTPRequestHandler):
    """Serves one in-memory payload with HTTP Range support.

    Handles a single byte-range per request: ``bytes=start-end``,
    ``bytes=start-`` and the suffix form ``bytes=-length``. Multi-range
    requests and unparsable specs get a 416.
    """

    protocol_version = "HTTP/1.1"
    payload: bytes = b""

    def log_message(self, format: str, *args: object) -> None:
        """Keep pytest output clean (stdlib hook, stdlib signature)."""

    def send_full_headers(self) -> bytes:
        data = type(self).payload
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "application/pdf")
        self.end_headers()
        return data

    def parse_range(self, spec: str, size: int) -> tuple[int, int] | None:
        """Parses one byte-range spec into inclusive (start, end) bounds."""
        first, sep, last = spec.strip().partition("-")
        if not sep or "," in spec:
            return None
        if first == "":
            if not last.isdigit() or int(last) == 0:
                return None
            return (max(size - int(last), 0), size - 1)
        if not first.isdigit():
            return None
        start = int(first)
        if start >= size:
            return None
        if last == "":
            return (start, size - 1)
        if not last.isdigit():
            return None
        return (start, min(int(last), size - 1))

    def do_HEAD(self) -> None:
        self.send_full_headers()

    def do_GET(self) -> None:
        data = type(self).payload
        header = self.headers.get("Range")
        if header is None:
            self.wfile.write(self.send_full_headers())
            return
        unit, sep, spec = header.partition("=")
        bounds = None
        if sep and unit.strip() == "bytes":
            bounds = self.parse_range(spec, len(data))
        if bounds is None:
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{len(data)}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        start, end = bounds
        body = data[start : end + 1]
        self.send_response(206)
        self.send_header("Content-Range", f"bytes {start}-{end}/{len(data)}")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "application/pdf")
        self.end_headers()
        self.wfile.write(body)


class NoRangeRequestHandler(BaseHTTPRequestHandler):
    """Ignores Range entirely: always answers 200 with the full payload."""

    protocol_version = "HTTP/1.1"
    payload: bytes = b""

    def log_message(self, format: str, *args: object) -> None:
        """Keep pytest output clean (stdlib hook, stdlib signature)."""

    def do_HEAD(self) -> None:
        self.send_response(200)
        self.send_header("Content-Length", str(len(type(self).payload)))
        self.send_header("Content-Type", "application/pdf")
        self.end_headers()

    def do_GET(self) -> None:
        data = type(self).payload
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Content-Type", "application/pdf")
        self.end_headers()
        self.wfile.write(data)


def serve(handler: type[BaseHTTPRequestHandler]) -> Iterator[str]:
    """Runs `handler` on a background ThreadingHTTPServer; yields the URL."""
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}/doc.pdf"
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


@pytest.fixture
def range_server(hello_pdf: Path) -> Iterator[str]:
    handler = type(
        "HelloRangeHandler",
        (RangeRequestHandler,),
        {"payload": hello_pdf.read_bytes()},
    )
    yield from serve(handler)


@pytest.fixture
def no_range_server(hello_pdf: Path) -> Iterator[str]:
    handler = type(
        "HelloNoRangeHandler",
        (NoRangeRequestHandler,),
        {"payload": hello_pdf.read_bytes()},
    )
    yield from serve(handler)


class TestOpenUrl:
    @pytest.mark.asyncio
    async def test_open_url_over_range_requests(
        self, range_server: str, hello_pdf: Path
    ) -> None:
        doc = await AsyncDocument.open_url(range_server)
        assert doc.page_count() == 1
        assert doc.version() == Document(str(hello_pdf)).version

    @pytest.mark.asyncio
    async def test_open_url_element_parity(
        self, range_server: str, hello_pdf: Path
    ) -> None:
        expected = [element_key(e) for e in Document(str(hello_pdf)).elements()]
        doc = await AsyncDocument.open_url(range_server)
        got = []
        async for element in doc.elements():
            got.append(element_key(element))
        assert got == expected

    @pytest.mark.asyncio
    async def test_get_object_over_http(
        self, range_server: str, hello_pdf: Path
    ) -> None:
        trailer = next(
            e
            for e in Document(str(hello_pdf)).elements(logical=False)
            if e.kind == "trailer"
        )
        num, gen = trailer.value()["Root"]["ref"]
        doc = await AsyncDocument.open_url(range_server)
        catalog = await doc.get_object(num, gen)
        assert catalog["Type"] == "Catalog"

    @pytest.mark.asyncio
    async def test_server_without_range_support_raises_http_error(
        self, no_range_server: str
    ) -> None:
        with pytest.raises(PdfError) as exc:
            await AsyncDocument.open_url(no_range_server)
        assert str(exc.value).startswith("http:")

    @pytest.mark.asyncio
    async def test_unreachable_url_raises_http_error(self) -> None:
        with pytest.raises(PdfError) as exc:
            await AsyncDocument.open_url("http://127.0.0.1:9/doc.pdf")
        assert str(exc.value).startswith("http:")
