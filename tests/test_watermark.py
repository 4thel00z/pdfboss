"""`pdfboss.write.watermark`: an incremental update that draws an overlay page over every page."""

import pdfboss
import pytest
from pdfboss.write import Page, Pdf, Standard14, Text, watermark


def two_page_base() -> bytes:
    first = Page(size="a4") | Text("Base page one", at=(72, 700))
    second = Page(size="a4") | Text("Base page two", at=(72, 700))
    return (Pdf() | first | second).to_bytes()


def overlay() -> bytes:
    page = Page(size="a4") | Text("DRAFT", at=(200, 400), font=Standard14.HELVETICA_BOLD, size=48)
    return (Pdf() | page).to_bytes()


def test_watermark_keeps_the_base_bytes_and_draws_the_overlay_on_every_page() -> None:
    base = two_page_base()
    out = watermark(base, overlay())
    assert out.startswith(base)
    assert len(out) < len(base) + 4096
    doc = pdfboss.Document(data=out)
    assert doc.page_count == 2
    for index, expected in enumerate(["Base page one", "Base page two"]):
        text = doc[index].extract_text()
        assert expected in text
        assert "DRAFT" in text


def test_watermark_rewrite_writes_one_fresh_section() -> None:
    base = two_page_base()
    out = watermark(base, overlay(), rewrite=True)
    assert not out.startswith(base)
    doc = pdfboss.Document(data=out)
    assert doc.page_count == 2
    for index, expected in enumerate(["Base page one", "Base page two"]):
        text = doc[index].extract_text()
        assert expected in text
        assert "DRAFT" in text


def test_watermark_refuses_bytes_that_are_not_a_pdf() -> None:
    with pytest.raises(pdfboss.PdfError):
        watermark(b"not a pdf", overlay())


def test_watermark_under_keeps_the_base_bytes_and_draws_both_texts() -> None:
    base = two_page_base()
    out = watermark(base, overlay(), under=True)
    assert out.startswith(base)
    doc = pdfboss.Document(data=out)
    assert doc.page_count == 2
    for index, expected in enumerate(["Base page one", "Base page two"]):
        text = doc[index].extract_text()
        assert expected in text
        assert "DRAFT" in text


def test_watermark_under_differs_from_the_default_result() -> None:
    base = two_page_base()
    mark = overlay()
    assert watermark(base, mark, under=True) != watermark(base, mark)


def test_watermark_under_rewrite_writes_one_fresh_section() -> None:
    base = two_page_base()
    out = watermark(base, overlay(), under=True, rewrite=True)
    assert not out.startswith(base)
    doc = pdfboss.Document(data=out)
    assert doc.page_count == 2
    for index, expected in enumerate(["Base page one", "Base page two"]):
        text = doc[index].extract_text()
        assert expected in text
        assert "DRAFT" in text
