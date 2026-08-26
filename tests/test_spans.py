"""Tests for styled span iteration: ``Page.spans``, ``Document.spans`` and
their async twins.

Runs against the committed fixture PDFs in ``tests/fixtures/`` plus small
in-memory documents built here. Requires the extension module to be built
and installed (e.g. via maturin).
"""

from collections.abc import Iterator
from pathlib import Path

import pytest

from pdfboss import AsyncDocument, Document, Span


def build_pdf(objects: dict[int, bytes]) -> bytes:
    """A classic-xref PDF from numbered object bodies (streams included)."""
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


def stream(dict_body: bytes, data: bytes) -> bytes:
    return b"<< %s /Length %d >>\nstream\n%s\nendstream" % (
        dict_body,
        len(data),
        data,
    )


@pytest.fixture
def styled_pdf() -> bytes:
    """One page exercising every style channel: a bold-italic descriptor
    font, a red fill, an underline ruling, and an invisible (``3 Tr``)
    run."""
    content = (
        b"BT /F1 12 Tf 72 720 Td (plain) Tj ET "
        b"BT /F2 12 Tf 72 690 Td 1 0 0 rg (styled) Tj 3 Tr ( ocr) Tj ET "
        b"72 688.5 m 110 688.5 l S"
    )
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                b"/Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> "
                b"/Contents 4 0 R >>"
            ),
            4: stream(b"", content),
            5: (
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica "
                b"/Encoding /WinAnsiEncoding >>"
            ),
            6: (
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Custom-Face "
                b"/Encoding /WinAnsiEncoding /FontDescriptor 7 0 R >>"
            ),
            7: (
                b"<< /Type /FontDescriptor /FontName /Custom-Face "
                b"/Flags 66 /FontWeight 700 /Ascent 718 /Descent -207 >>"
            ),
        }
    )


class TestPageSpans:
    def test_returns_span_list(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        spans = doc[0].spans()
        assert isinstance(spans, list)
        assert all(isinstance(s, Span) for s in spans)
        assert spans[0].text == "Hello, world!"

    def test_geometry_and_identity(self, hello_pdf: Path) -> None:
        (span,) = Document(str(hello_pdf))[0].spans()
        x0, y0, x1, y1 = span.bbox
        assert x0 < x1 and y0 < y1
        assert x0 == pytest.approx(span.x)
        assert y0 < span.y < y1
        assert span.size > 0.0
        assert span.page == 0
        assert span.font == "F1"
        assert span.font_name != ""

    def test_style_attributes(self, styled_pdf: bytes) -> None:
        doc = Document(data=styled_pdf)
        plain, styled, ocr = doc[0].spans()
        assert plain.text == "plain"
        assert not plain.bold and not plain.italic
        assert not plain.underline and not plain.strikethrough
        assert plain.color == pytest.approx((0.0, 0.0, 0.0))
        assert not plain.invisible

        assert styled.text == "styled"
        assert styled.font_name == "Custom-Face"
        assert styled.bold and styled.italic
        assert styled.serif and not styled.monospace
        assert styled.underline and not styled.strikethrough
        assert styled.color == pytest.approx((1.0, 0.0, 0.0))
        assert not styled.vertical
        assert styled.rise == 0.0

        assert ocr.invisible

    def test_hidden_layers_are_excluded(self) -> None:
        data = build_pdf(
            {
                1: (
                    b"<< /Type /Catalog /Pages 2 0 R /OCProperties "
                    b"<< /OCGs [6 0 R] /D << /OFF [6 0 R] >> >> >>"
                ),
                2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
                3: (
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                    b"/Resources << /Font << /F1 5 0 R >> "
                    b"/Properties << /H 6 0 R >> >> /Contents 4 0 R >>"
                ),
                4: stream(
                    b"",
                    b"BT /F1 12 Tf 72 720 Td /OC /H BDC (hidden) Tj EMC (kept) Tj ET",
                ),
                5: (
                    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica "
                    b"/Encoding /WinAnsiEncoding >>"
                ),
                6: b"<< /Type /OCG /Name (hidden) >>",
            }
        )
        doc = Document(data=data)
        assert [s.text for s in doc[0].spans()] == ["kept"]


class TestDocumentSpans:
    def test_lazy_iterator_in_page_order(self, three_pages_pdf: Path) -> None:
        doc = Document(str(three_pages_pdf))
        spans = doc.spans()
        assert isinstance(spans, Iterator)
        pages = [s.page for s in spans]
        assert pages == sorted(pages)
        assert set(pages) == {0, 1, 2}

    def test_pages_filter_in_given_order(self, three_pages_pdf: Path) -> None:
        doc = Document(str(three_pages_pdf))
        pages = [s.page for s in doc.spans(pages=[2, 0])]
        assert pages == [2, 0]

    def test_matches_per_page_extraction(self, three_pages_pdf: Path) -> None:
        doc = Document(str(three_pages_pdf))
        walked = [s.text for s in doc.spans()]
        per_page = [s.text for i in range(len(doc)) for s in doc[i].spans()]
        assert walked == per_page


class TestAsyncSpans:
    @pytest.mark.asyncio
    async def test_page_spans_match_sync(self, hello_pdf: Path) -> None:
        doc = await AsyncDocument.open(hello_pdf)
        spans = await doc[0].spans()
        sync_spans = Document(str(hello_pdf))[0].spans()
        assert [s.text for s in spans] == [s.text for s in sync_spans]
        assert spans[0].bbox == sync_spans[0].bbox
        assert spans[0].font_name == sync_spans[0].font_name

    @pytest.mark.asyncio
    async def test_document_spans_is_async_iterator(
        self, three_pages_pdf: Path
    ) -> None:
        doc = await AsyncDocument.open(three_pages_pdf)
        pages = [s.page async for s in doc.spans()]
        assert set(pages) == {0, 1, 2}

    @pytest.mark.asyncio
    async def test_document_spans_pages_filter(self, three_pages_pdf: Path) -> None:
        doc = await AsyncDocument.open(three_pages_pdf)
        pages = [s.page async for s in doc.spans(pages=[1])]
        assert pages == [1]

    @pytest.mark.asyncio
    async def test_hidden_layers_match_sync(self, styled_pdf: bytes) -> None:
        doc = await AsyncDocument.from_bytes(styled_pdf)
        spans = await doc[0].spans()
        sync_spans = Document(data=styled_pdf)[0].spans()
        assert [s.text for s in spans] == [s.text for s in sync_spans]
        assert [s.underline for s in spans] == [s.underline for s in sync_spans]
        assert [s.color for s in spans] == [s.color for s in sync_spans]
