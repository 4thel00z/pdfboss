"""The invisible_text flag: extraction reads the page box by default and
keeps off-page (pasteboard) content only on request."""

import pdfboss
from pdfboss.write import Page, Pdf, Text


def pasteboard_pdf() -> bytes:
    """An a5 page with one visible line and one line far right of the page
    box, the way a page cropped out of a larger document keeps its
    neighbors' text in the stream."""
    page = Page(size="a5") | Text("inside", at=(72, 400)) | Text("pasteboard", at=(900, 400))
    return (Pdf() | page).to_bytes()


def test_extraction_clips_to_the_page_box() -> None:
    doc = pdfboss.Document(data=pasteboard_pdf())
    assert doc[0].extract_text() == "inside"
    assert doc.extract_text() == "inside"
    assert "pasteboard" not in doc.extract_markdown()
    assert "pasteboard" not in doc[0].extract_markdown()


def test_invisible_text_keeps_off_page_content() -> None:
    doc = pdfboss.Document(data=pasteboard_pdf())
    assert doc[0].extract_text(invisible_text=True) == "inside pasteboard"
    assert doc.extract_text(invisible_text=True) == "inside pasteboard"
    assert "pasteboard" in doc.extract_markdown(invisible_text=True)
    assert "pasteboard" in doc[0].extract_markdown(invisible_text=True)


def test_spans_stay_unclipped() -> None:
    doc = pdfboss.Document(data=pasteboard_pdf())
    texts = [span.text for span in doc[0].spans()]
    assert "pasteboard" in texts
