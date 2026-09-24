"""Tests for ``Page.content_items``: a tagged page's marked-content
sequences and the annotations the structure tree holds through object
references (ISO 32000-1 14.7.4.3), ranked together in tree order
(14.8.2.3.2), sync and async.
"""

import asyncio

import pdfboss
from pdfboss import AsyncDocument, Document
from test_spans import build_pdf, stream

CONTENT = b"""BT /F1 12 Tf
/P << /MCID 0 >> BDC 1 0 0 1 72 700 Tm (Before) Tj EMC
/P << /MCID 1 >> BDC 1 0 0 1 72 680 Tm (After) Tj EMC ET"""


def linked_pdf() -> bytes:
    """One tagged page: paragraph 9 holds sequence 0, the Link element 10
    holds the annotation 12 through an object reference, paragraph 11
    holds sequence 1. The parent tree maps key 0 to the page's array and
    key 1, the annotation's ``/StructParent``, to the Link element.
    Object numbers stay contiguous, which ``build_pdf`` relies on."""
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 6 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /StructParents 0 "
                b"/Annots [12 0 R 13 0 R] "
                b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            ),
            4: stream(b"", CONTENT),
            5: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            6: b"<< /Type /StructTreeRoot /K [7 0 R] /ParentTree 8 0 R >>",
            7: b"<< /Type /StructElem /S /Document /P 6 0 R /K [9 0 R 10 0 R 11 0 R] >>",
            8: b"<< /Nums [0 [9 0 R 11 0 R] 1 10 0 R] >>",
            9: b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [0] >>",
            10: (
                b"<< /Type /StructElem /S /Link /P 7 0 R /Pg 3 0 R /Alt (Home page) "
                b"/K [<< /Type /OBJR /Obj 12 0 R >>] >>"
            ),
            11: b"<< /Type /StructElem /S /P /P 7 0 R /Pg 3 0 R /K [1] >>",
            12: (
                b"<< /Type /Annot /Subtype /Link /Rect [72 690 200 710] /StructParent 1 "
                b"/A << /S /URI /URI (https://example.com) >> >>"
            ),
            13: b"<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] >>",
        }
    )


def untagged_pdf() -> bytes:
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            ),
            4: stream(b"", CONTENT),
            5: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        }
    )


def test_an_annotation_takes_its_place_between_the_sequences() -> None:
    page = Document(data=linked_pdf())[0]
    items = page.content_items()
    assert [(item.kind, item.rank) for item in items] == [
        ("sequence", 0),
        ("object", 1),
        ("sequence", 2),
    ]
    before, link, after = items
    assert (before.mcid, before.struct_parents, before.ref) == (0, 0, None)
    assert (after.mcid, after.struct_parents) == (1, 0)
    assert before.standard_type == "P"
    assert (link.mcid, link.struct_parents) == (None, None)
    assert link.ref == (12, 0)
    assert link.structure_type == "Link"
    assert link.mapped_type == "Link"
    assert link.standard_type == "Link"
    assert link.path == [("Document", (7, 0)), ("Link", (10, 0))]
    assert link.alt == "Home page"
    assert link.lang is None
    assert link.expansion is None
    assert repr(link) == "ContentItem(kind='object', ref=(12, 0), rank=1, structure_type='Link')"
    assert repr(before) == "ContentItem(kind='sequence', mcid=0, rank=0, structure_type='P')"


def test_the_item_matches_the_annotation_by_reference() -> None:
    page = Document(data=linked_pdf())[0]
    by_ref = {annotation.ref: annotation for annotation in page.annotations()}
    (link,) = [item for item in page.content_items() if item.kind == "object"]
    annotation = by_ref[link.ref]
    assert annotation.subtype == "Link"
    assert annotation.struct_parent == 1
    assert annotation.action.uri == "https://example.com"
    # The Square annotation carries no /StructParent, so the tree does not
    # hold it and it is no content item.
    assert (13, 0) in by_ref


def test_an_untagged_page_has_no_content_items() -> None:
    assert Document(data=untagged_pdf())[0].content_items() == []


def test_the_class_is_exported() -> None:
    assert pdfboss.ContentItem.__name__ == "ContentItem"


def test_async_twin_agrees_with_sync() -> None:
    async def run() -> list[tuple[str, int | None, tuple[int, int] | None, int]]:
        doc = await AsyncDocument.from_bytes(linked_pdf())
        items = await doc[0].content_items()
        return [(item.kind, item.mcid, item.ref, item.rank) for item in items]

    assert asyncio.run(run()) == [
        (item.kind, item.mcid, item.ref, item.rank)
        for item in Document(data=linked_pdf())[0].content_items()
    ]
