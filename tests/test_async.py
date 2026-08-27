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

import pdfboss
from pdfboss import AsyncDocument, Document, Element, PdfError


class TestAsyncOpen:
    @pytest.mark.asyncio
    async def test_open_by_pathlike(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(hello_pdf)
        assert doc.page_count == 1

    @pytest.mark.asyncio
    async def test_open_by_str(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(str(hello_pdf))
        assert doc.page_count == 1

    @pytest.mark.asyncio
    async def test_from_bytes(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.from_bytes(hello_pdf.read_bytes())
        assert doc.page_count == 1

    @pytest.mark.asyncio
    async def test_version_matches_sync(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(hello_pdf)
        assert doc.version == Document(str(hello_pdf)).version

    @pytest.mark.asyncio
    async def test_xref_stream_file_opens(self, xref_stream_pdf: Path) -> None:
        doc = await AsyncDocument.open(xref_stream_pdf)
        assert doc.page_count == 1

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
        assert [d.page_count for d in docs] == [1, 3]

    @pytest.mark.asyncio
    async def test_page_boxes_match_sync(self, boxed_pdf: bytes) -> None:
        sync_page = Document(data=boxed_pdf)[0]
        page = (await AsyncDocument.from_bytes(boxed_pdf))[0]
        assert page.media_box == sync_page.media_box
        assert page.crop_box == sync_page.crop_box
        assert page.bleed_box == sync_page.bleed_box
        assert page.trim_box == sync_page.trim_box
        assert page.art_box == sync_page.art_box
        assert page.trim_box == pytest.approx((60.0, 70.0, 540.0, 730.0))
        assert page.art_box == page.crop_box, "undeclared art box is the crop box"


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
def three_pages_range_server(three_pages_pdf: Path) -> Iterator[tuple[str, Path]]:
    handler = type(
        "ThreePagesRangeHandler",
        (RangeRequestHandler,),
        {"payload": three_pages_pdf.read_bytes()},
    )
    for url in serve(handler):
        yield url, three_pages_pdf


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
        assert doc.page_count == 1
        assert doc.version == Document(str(hello_pdf)).version

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


class TestAsyncPageParity:
    """The async page surface must agree with the sync one byte for byte:
    same attributes, same extracted text, same rendered PNG."""

    @pytest.mark.asyncio
    async def test_page_attributes_match_sync(self, hello_pdf):
        sync_doc = Document(str(hello_pdf))
        doc = await AsyncDocument.open(str(hello_pdf))
        assert len(doc) == sync_doc.page_count
        for i in range(len(doc)):
            sync_page = sync_doc[i]
            page = doc[i]
            assert page.number == sync_page.number
            assert page.width == sync_page.width
            assert page.height == sync_page.height
            assert page.rotation == sync_page.rotation

    @pytest.mark.asyncio
    async def test_negative_index_counts_from_the_end(self, hello_pdf):
        doc = await AsyncDocument.open(str(hello_pdf))
        assert doc[-1].number == len(doc) - 1
        with pytest.raises(IndexError):
            doc[len(doc)]
        with pytest.raises(pdfboss.PdfError):
            doc.page(len(doc))

    @pytest.mark.asyncio
    async def test_extract_text_matches_sync(self, hello_pdf):
        sync_doc = Document(str(hello_pdf))
        doc = await AsyncDocument.open(str(hello_pdf))
        assert await doc.extract_text() == sync_doc.extract_text()
        assert await doc[0].extract_text() == sync_doc[0].extract_text()

    @pytest.mark.asyncio
    async def test_render_matches_sync_byte_for_byte(self, hello_pdf):
        sync_doc = Document(str(hello_pdf))
        doc = await AsyncDocument.open(str(hello_pdf))
        sync_png = sync_doc[0].render(scale=1.5)
        png = await doc[0].render(scale=1.5)
        assert png == sync_png
        png2, warnings = await doc[0].render_reporting(scale=1.5)
        sync_png2, sync_warnings = sync_doc[0].render_reporting(scale=1.5)
        assert png2 == sync_png2
        assert warnings == sync_warnings

    @pytest.mark.asyncio
    async def test_render_pages_matches_sync_byte_for_byte(self, three_pages_pdf):
        sync_doc = Document(str(three_pages_pdf))
        doc = await AsyncDocument.open(str(three_pages_pdf))
        assert await doc.render_pages(scale=1.5) == sync_doc.render_pages(scale=1.5)

    @pytest.mark.asyncio
    async def test_render_pages_honors_an_explicit_selection(self, three_pages_pdf):
        sync_doc = Document(str(three_pages_pdf))
        doc = await AsyncDocument.open(str(three_pages_pdf))
        subset = await doc.render_pages(pages=[2, 0], scale=1.0)
        assert subset == sync_doc.render_pages(pages=[2, 0], scale=1.0)

    @pytest.mark.asyncio
    async def test_render_pages_works_over_http(self, three_pages_range_server):
        # The fan-out must hold over a range-fetching source too: the
        # workers share one HTTP-backed document and still agree with the
        # sync render of the same bytes.
        url, pdf_path = three_pages_range_server
        sync_doc = Document(str(pdf_path))
        doc = await AsyncDocument.open_url(url)
        assert await doc.render_pages() == sync_doc.render_pages()


class TestAsyncRenderCompression:
    @pytest.mark.asyncio
    async def test_every_level_matches_the_sync_bytes(self, hello_pdf: Path) -> None:
        sync_page = Document(str(hello_pdf))[0]
        doc = await AsyncDocument.open(str(hello_pdf))
        for level in ("none", "fast", "default", "best"):
            png = await doc[0].render(compression=level)
            assert png == sync_page.render(compression=level), level

    @pytest.mark.asyncio
    async def test_omitted_compression_is_byte_identical_to_the_default_level(
        self, hello_pdf: Path
    ) -> None:
        doc = await AsyncDocument.open(str(hello_pdf))
        page = doc[0]
        assert await page.render() == await page.render(compression="default")
        reporting = await page.render_reporting()
        explicit = await page.render_reporting(compression="default")
        assert reporting[0] == explicit[0]
        assert await doc.render_pages() == await doc.render_pages(compression="default")

    @pytest.mark.asyncio
    async def test_unknown_compression_raises_value_error_naming_the_choices(
        self, hello_pdf: Path
    ) -> None:
        doc = await AsyncDocument.open(str(hello_pdf))
        page = doc[0]
        for call in (page.render, page.render_reporting, doc.render_pages):
            with pytest.raises(ValueError, match="'none', 'fast', 'default' or 'best'"):
                await call(compression="bogus")


class TestAsyncExtractImages:
    @pytest.mark.asyncio
    async def test_matches_the_sync_extraction(self, image_pdf: bytes) -> None:
        doc = await AsyncDocument.from_bytes(image_pdf)
        images = await doc[0].extract_images()
        sync_images = Document(data=image_pdf)[0].extract_images()
        assert [(i.width, i.height, i.data) for i in images] == [
            (i.width, i.height, i.data) for i in sync_images
        ]
        assert len(images) == 1
        assert images[0].data[:8] == b"\x89PNG\r\n\x1a\n"
