"""Tests for layout blocks: ``Document.layout``, ``Page.layout`` and their
async twins, over documents composed in memory with ``pdfboss.md``."""

from pathlib import Path

import pytest

import pdfboss
from pdfboss import AsyncDocument, Document, LayoutBlock

PROSE = " ".join(["the paragraph keeps going with plain words"] * 6)

REPORT = f"""# Annual summary

{PROSE}

{PROSE}

- first point
- second point

| a | b | c |
|---|---|---|
| 1 | 2 | 3 |
| 4 | 5 | 6 |
| 7 | 8 | 9 |
"""


@pytest.fixture
def report() -> bytes:
    return pdfboss.md.to_pdf(REPORT)


def kinds(blocks: list[LayoutBlock]) -> list[str]:
    return [block.kind for block in blocks]


def test_document_layout_reads_every_block_kind(report: bytes) -> None:
    (blocks,) = Document(data=report).layout()
    assert kinds(blocks) == ["heading", "paragraph", "list", "table"]
    heading = blocks[0]
    assert heading.level == 1
    assert heading.role is None
    assert heading.text == "Annual summary"
    assert blocks[1].role == "body"
    assert blocks[1].level is None


def test_blocks_lie_inside_the_media_box(report: bytes) -> None:
    doc = Document(data=report)
    x0, y0, x1, y1 = doc[0].media_box
    for block in doc.layout()[0]:
        bx0, by0, bx1, by1 = block.bbox
        assert x0 <= bx0 < bx1 <= x1, block
        assert y0 <= by0 < by1 <= y1, block


def test_list_items_carry_marker_text_and_box(report: bytes) -> None:
    (blocks,) = Document(data=report).layout()
    (listing,) = [block for block in blocks if block.kind == "list"]
    assert [item.text for item in listing.items] == ["first point", "second point"]
    assert all(item.marker for item in listing.items)
    first, second = listing.items
    assert first.bbox[1] > second.bbox[1]
    assert listing.bbox[1] <= second.bbox[1]
    assert first.bbox[3] <= listing.bbox[3]


def test_table_rows_carry_cells(report: bytes) -> None:
    (blocks,) = Document(data=report).layout()
    (table,) = [block for block in blocks if block.kind == "table"]
    assert [[cell.text for cell in row] for row in table.rows] == [
        ["a", "b", "c"],
        ["1", "2", "3"],
        ["4", "5", "6"],
        ["7", "8", "9"],
    ]
    assert all(cell.colspan == 1 and cell.rowspan == 1 for row in table.rows for cell in row)
    assert table.items == []


def test_page_layout_matches_the_one_page_document(report: bytes) -> None:
    doc = Document(data=report)
    assert kinds(doc[0].layout()) == kinds(doc.layout()[0])


def test_pages_selects_in_the_order_given(three_pages_pdf: Path) -> None:
    doc = Document(three_pages_pdf)
    assert [page[0].text for page in doc.layout()] == ["Page one", "Page two", "Page three"]
    last, first = doc.layout(pages=[-1, 0])
    assert last[0].text == "Page three"
    assert first[0].text == "Page one"
    with pytest.raises(IndexError):
        doc.layout(pages=[3])


def test_paragraph_gap_changes_the_paragraphs(report: bytes) -> None:
    doc = Document(data=report)
    assert kinds(doc.layout()[0]).count("paragraph") == 1
    assert kinds(doc.layout(paragraph_gap=1.4)[0]).count("paragraph") == 2


def test_every_knob_is_accepted(report: bytes) -> None:
    blocks = Document(data=report).layout(
        line_overlap=0.5,
        redraw_tolerance=0.1,
        paragraph_gap=1.6,
        paragraph_pitch="lower_half_median",
        min_pitch_steps=4,
        flow_step_up=2.0,
        flow_line_up=1.0,
        flow_step_aside=2.0,
        heading_min_delta=1.0,
        heading_max_chars=120,
        bold_heading_max_chars=72,
        heading_merge_step=1.8,
        list_min_lines=2,
        list_continuation_indent=0.5,
        page_title_min_gap=1.5,
        header_footer_min_pages=3,
    )[0]
    assert blocks[0].kind == "heading"


def test_a_raised_heading_delta_demotes_the_heading(report: bytes) -> None:
    blocks = Document(data=report)[0].layout(heading_min_delta=100.0)
    assert "heading" not in kinds(blocks)


def test_unknown_knob_is_a_type_error(report: bytes) -> None:
    with pytest.raises(TypeError, match="paragraph_gapp"):
        Document(data=report).layout(paragraph_gapp=1.6)


def test_unknown_pitch_is_a_value_error(report: bytes) -> None:
    with pytest.raises(ValueError, match="paragraph_pitch"):
        Document(data=report)[0].layout(paragraph_pitch="mean")


@pytest.mark.asyncio
async def test_async_document_layout_matches_sync(report: bytes) -> None:
    doc = await AsyncDocument.from_bytes(report)
    blocks = await doc.layout()
    sync = Document(data=report).layout()
    assert [kinds(page) for page in blocks] == [kinds(page) for page in sync]
    assert [block.bbox for block in blocks[0]] == [block.bbox for block in sync[0]]
    (selected,) = await doc.layout(pages=[-1], paragraph_gap=1.4)
    assert kinds(selected).count("paragraph") == 2


@pytest.mark.asyncio
async def test_async_page_layout_matches_sync(report: bytes) -> None:
    doc = await AsyncDocument.from_bytes(report)
    blocks = await doc.page(0).layout(heading_min_delta=100.0)
    assert kinds(blocks) == kinds(Document(data=report)[0].layout(heading_min_delta=100.0))
