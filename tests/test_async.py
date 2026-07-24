"""Tests for AsyncDocument: async open, metadata and object fetch.

Runs against the committed fixture PDFs in ``tests/fixtures/``. Requires the
extension module to be built and installed (e.g. via maturin).
"""

import asyncio
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
