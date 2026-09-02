"""Tests for pdfboss.write's assembly verbs: merge, split, rotate and
rewrite. Thin bytes-in/bytes-out wrappers over the underlying library
functions, with 0-based page lists throughout (the 1-based convention is
CLI-only)."""

import pytest

import pdfboss
from pdfboss.write import Page, Pdf, Text, Update, merge, rewrite, rotate, split


def build_pdf(*texts: str) -> bytes:
    pdf = Pdf()
    for text in texts:
        pdf = pdf | (Page(size="a4") | Text(text, at=(72, 700)))
    return pdf.to_bytes()


def page_texts(data: bytes) -> list[str]:
    doc = pdfboss.Document(data=data)
    return [doc[i].extract_text() for i in range(doc.page_count)]


def xref_section_count(data: bytes) -> int:
    doc = pdfboss.Document(data=data)
    return sum(1 for element in doc.elements(physical=True, logical=False) if element.kind == "xref")


def test_merge_gathers_every_source_page_in_argument_order() -> None:
    a = build_pdf("a1", "a2")
    b = build_pdf("b1", "b2")
    merged = merge([a, b])
    assert pdfboss.Document(data=merged).page_count == 4
    texts = page_texts(merged)
    assert "a1" in texts[0]
    assert "a2" in texts[1]
    assert "b1" in texts[2]
    assert "b2" in texts[3]


def test_merge_selects_and_reorders_pages() -> None:
    a = build_pdf("one", "two", "three")
    merged = merge([(a, [2, 0])])
    texts = page_texts(merged)
    assert len(texts) == 2
    assert "three" in texts[0]
    assert "one" in texts[1]


def test_merge_mixes_whole_and_selected_inputs() -> None:
    a = build_pdf("one", "two")
    b = build_pdf("three")
    merged = merge([(a, [1]), b])
    texts = page_texts(merged)
    assert len(texts) == 2
    assert "two" in texts[0]
    assert "three" in texts[1]


def test_split_round_trips_page_counts() -> None:
    data = build_pdf("one", "two", "three")
    parts = split(data, 2)
    assert len(parts) == 2
    assert pdfboss.Document(data=parts[0]).page_count == 2
    assert pdfboss.Document(data=parts[1]).page_count == 1


def test_rotate_append_prefixes_the_input_and_updates_rotation() -> None:
    data = build_pdf("one", "two")
    rotated = rotate(data, 90, pages=[0])
    assert rotated.startswith(data)
    doc = pdfboss.Document(data=rotated)
    assert doc[0].rotation == 90
    assert doc[1].rotation == 0


def test_rotate_rewrite_updates_rotation_without_prefixing_the_input() -> None:
    data = build_pdf("one", "two")
    rotated = rotate(data, 90, pages=[0], rewrite=True)
    assert not rotated.startswith(data)
    doc = pdfboss.Document(data=rotated)
    assert doc[0].rotation == 90
    assert doc[1].rotation == 0


def test_rotate_defaults_to_every_page() -> None:
    data = build_pdf("one", "two")
    rotated = rotate(data, 180)
    doc = pdfboss.Document(data=rotated)
    assert doc[0].rotation == 180
    assert doc[1].rotation == 180


def test_rotate_rejects_an_unsupported_angle() -> None:
    data = build_pdf("one")
    with pytest.raises(ValueError, match="90, 180 or 270"):
        rotate(data, 45)


def test_rewrite_collapses_an_appended_update_chain() -> None:
    base = build_pdf("one")
    update = Update(pdfboss.Document(data=base))
    update.set_metadata(title="Chained")
    appended = update.to_bytes()
    assert xref_section_count(appended) == 2

    rewritten = rewrite(appended)
    assert xref_section_count(rewritten) == 1
    assert pdfboss.Document(data=rewritten).metadata.get("title") == "Chained"
