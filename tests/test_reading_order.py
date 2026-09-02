"""Tests for the ``reading_order`` keyword: the three orders on a tagged
two-column page, on every extraction method, sync and async.

The page is written row by row with the lower half first, so the content
stream, the page geometry and the structure tree each read it differently.
"""

import asyncio

import pytest

import pdfboss
from pdfboss import AsyncDocument, Document, ReadingOrder
from test_spans import build_pdf, stream

CONTENT = b"""BT /F1 12 Tf
/P << /MCID 0 >> BDC 1 0 0 1 72 660 Tm (L3) Tj EMC
/P << /MCID 1 >> BDC 1 0 0 1 300 660 Tm (R3) Tj EMC
/P << /MCID 2 >> BDC 1 0 0 1 72 640 Tm (L4) Tj EMC
/P << /MCID 3 >> BDC 1 0 0 1 300 640 Tm (R4) Tj EMC
/P << /MCID 4 >> BDC 1 0 0 1 72 700 Tm (L1) Tj EMC
/P << /MCID 5 >> BDC 1 0 0 1 300 700 Tm (R1) Tj EMC
/P << /MCID 6 >> BDC 1 0 0 1 72 680 Tm (L2) Tj EMC
/P << /MCID 7 >> BDC 1 0 0 1 300 680 Tm (R2) Tj EMC ET"""

CONTENT_ORDER = "L3 R3\nL4 R4\nL1 R1\nL2 R2"
GEOMETRIC_ORDER = "L1 R1\nL2 R2\nL3 R3\nL4 R4"
TREE_ORDER = "L1\nL2\nL3\nL4\nR1\nR2\nR3\nR4"


@pytest.fixture
def tagged_pdf() -> bytes:
    """One tagged page: two paragraphs, the left holding marked content
    4 6 0 2 (L1 to L4), the right 5 7 1 3 (R1 to R4). The catalog names a
    structure tree root and no /MarkInfo. Object numbers stay contiguous,
    which ``build_pdf``'s xref table relies on."""
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 6 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 "
                b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            ),
            4: stream(b"", CONTENT),
            5: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            6: b"<< /Type /StructTreeRoot /K [7 0 R] /ParentTree 8 0 R >>",
            7: b"<< /Type /StructElem /S /Document /P 6 0 R /K [9 0 R 10 0 R] >>",
            8: b"<< /Nums [0 [9 0 R 10 0 R 9 0 R 10 0 R 9 0 R 10 0 R 9 0 R 10 0 R]] >>",
            9: b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [4 6 0 2] >>",
            10: b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [5 7 1 3] >>",
        }
    )


def labels(text: str) -> list[str]:
    return [t for t in text.split() if t[0] in "LR"]


class TestEnum:
    def test_values_are_the_wire_strings(self) -> None:
        assert ReadingOrder.CONTENT == "content"
        assert ReadingOrder.STRUCTURE_TREE == "structure-tree"
        assert ReadingOrder.GEOMETRIC == "geometric"
        assert "ReadingOrder" in pdfboss.__all__

    def test_unknown_order_raises(self, tagged_pdf: bytes) -> None:
        doc = Document(data=tagged_pdf)
        with pytest.raises(ValueError, match="unknown reading order"):
            doc.extract_text(reading_order="sideways")

    def test_order_is_keyword_only(self, tagged_pdf: bytes) -> None:
        doc = Document(data=tagged_pdf)
        with pytest.raises(TypeError):
            doc.extract_text("geometric")  # type: ignore[misc]


class TestDocument:
    def test_default_is_content_order(self, tagged_pdf: bytes) -> None:
        doc = Document(data=tagged_pdf)
        assert doc.extract_text() == CONTENT_ORDER
        assert doc.extract_text(reading_order=ReadingOrder.CONTENT) == CONTENT_ORDER

    def test_three_orders_on_document_and_page(self, tagged_pdf: bytes) -> None:
        doc = Document(data=tagged_pdf)
        for target in (doc, doc[0]):
            assert target.extract_text(reading_order="content") == CONTENT_ORDER
            assert target.extract_text(reading_order="geometric") == GEOMETRIC_ORDER
            assert target.extract_text(reading_order=ReadingOrder.STRUCTURE_TREE) == TREE_ORDER

    def test_markdown_follows_the_order(self, tagged_pdf: bytes) -> None:
        doc = Document(data=tagged_pdf)
        assert labels(doc.extract_markdown(reading_order="structure-tree")) == TREE_ORDER.split()
        assert labels(doc[0].extract_markdown(reading_order="geometric")) == GEOMETRIC_ORDER.split()

    def test_spans_come_in_the_order(self, tagged_pdf: bytes) -> None:
        doc = Document(data=tagged_pdf)
        assert [s.text for s in doc[0].spans()] == "L3 R3 L4 R4 L1 R1 L2 R2".split()
        assert [s.text for s in doc[0].spans(reading_order="structure-tree")] == TREE_ORDER.split()
        assert [s.text for s in doc.spans(reading_order="structure-tree")] == TREE_ORDER.split()

    def test_untagged_document_reads_the_same_in_tree_order(self, hello_pdf) -> None:
        doc = Document(hello_pdf)
        assert doc.extract_text(reading_order="structure-tree") == doc.extract_text()


class TestAsyncDocument:
    @pytest.mark.asyncio
    async def test_three_orders(self, tagged_pdf: bytes) -> None:
        doc = await AsyncDocument.from_bytes(tagged_pdf)
        assert await doc.extract_text() == CONTENT_ORDER
        assert await doc.extract_text(reading_order="geometric") == GEOMETRIC_ORDER
        assert await doc.extract_text(reading_order=ReadingOrder.STRUCTURE_TREE) == TREE_ORDER
        page = doc[0]
        assert await page.extract_text(reading_order="structure-tree") == TREE_ORDER
        assert labels(await page.extract_markdown(reading_order="structure-tree")) == TREE_ORDER.split()
        assert labels(await doc.extract_markdown(reading_order="geometric")) == GEOMETRIC_ORDER.split()

    @pytest.mark.asyncio
    async def test_spans_come_in_the_order(self, tagged_pdf: bytes) -> None:
        doc = await AsyncDocument.from_bytes(tagged_pdf)
        page_spans = await doc[0].spans(reading_order="structure-tree")
        assert [s.text for s in page_spans] == TREE_ORDER.split()
        texts = [s.text async for s in doc.spans(reading_order="structure-tree")]
        assert texts == TREE_ORDER.split()
