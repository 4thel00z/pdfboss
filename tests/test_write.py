"""Tests for `pdfboss.write`: the composition core (Task 9) and the full
vocabulary plus draw protocol (Task 10) — frozen pyclasses accumulated
with `|`, lowered once into a Rust `Pdf` at `save`/`to_bytes` time."""

import struct
import zlib
from pathlib import Path

import pytest

import pdfboss
from pdfboss.write import (
    Attachment,
    Bookmark,
    Canvas,
    Image,
    Link,
    Metadata,
    Outline,
    Page,
    PageLabel,
    Paragraph,
    Pdf,
    Standard14,
    Text,
    Viewer,
)


def make_gray8_png(width: int, height: int, pixel: int) -> bytes:
    """A minimal 8-bit grayscale PNG, built with the standard library only."""

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + tag
            + payload
            + struct.pack(">I", zlib.crc32(tag + payload))
        )

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)
    raw = b"".join(bytes([0]) + bytes([pixel]) * width for _ in range(height))
    idat = zlib.compress(raw)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def test_compose_and_save_bytes() -> None:
    page = Page(size="a4") | Text("Q3 Report", at=(72, 770), font=Standard14.HELVETICA_BOLD, size=28)
    data = (Pdf() | page | Metadata(title="Q3 Report")).to_bytes()
    assert data.startswith(b"%PDF-")


def test_slots_are_order_free() -> None:
    page = Page() | Text("same", at=(72, 700))
    first = (Pdf() | page | Metadata(title="fine")).to_bytes()
    second = (Pdf() | Metadata(title="fine") | page).to_bytes()
    assert first == second


def test_all_singleton_slots_are_order_free() -> None:
    page = Page() | Text("same", at=(72, 700))
    metadata = Metadata(title="fine")
    outline = Outline(Bookmark("Only", 0))
    viewer = Viewer(mode="use-none")

    slots_first = (Pdf() | metadata | outline | viewer | page).to_bytes()
    slots_last = (Pdf() | page | metadata | outline | viewer).to_bytes()
    shuffled = (Pdf() | viewer | page | metadata | outline).to_bytes()

    assert slots_first == slots_last == shuffled


def test_duplicate_slot_raises() -> None:
    with pytest.raises(TypeError, match="already has Metadata"):
        Pdf() | Metadata(title="a") | Metadata(title="b")


def test_or_returns_new_values() -> None:
    base = Pdf() | (Page() | Text("one", at=(72, 700)))
    with_meta = base | Metadata(title="t")
    assert base.to_bytes() != with_meta.to_bytes()


def test_pages_keep_order() -> None:
    first = Page() | Text("first", at=(72, 700))
    second = Page() | Text("second", at=(72, 700))
    ab = (Pdf() | first | second).to_bytes()
    ba = (Pdf() | second | first).to_bytes()
    assert ab != ba


def test_link_requires_exactly_one_target() -> None:
    with pytest.raises(TypeError):
        Link(rect=(0, 0, 1, 1))
    with pytest.raises(TypeError):
        Link(rect=(0, 0, 1, 1), url="https://x.y", page=0)


def test_reopen_and_extract_text(tmp_path: Path) -> None:
    page = Page(size="a4") | Text("Round trip content", at=(72, 700))
    data = (Pdf() | page).to_bytes()
    path = tmp_path / "roundtrip.pdf"
    path.write_bytes(data)
    doc = pdfboss.Document(str(path))
    assert "Round trip content" in doc.extract_text()


def test_to_bytes_is_callable_twice() -> None:
    page = Page() | Text("stable", at=(72, 700))
    pdf = Pdf() | page
    assert pdf.to_bytes() == pdf.to_bytes()


def test_empty_pdf_raises_pdf_error() -> None:
    with pytest.raises(pdfboss.PdfError, match="at least one page"):
        Pdf().to_bytes()


def test_save_writes_file(tmp_path: Path) -> None:
    page = Page() | Text("saved", at=(72, 700))
    path = tmp_path / "out.pdf"
    (Pdf() | page).save(str(path))
    assert path.read_bytes().startswith(b"%PDF-")


def test_page_or_rejects_unsupported_type() -> None:
    with pytest.raises(TypeError, match="int"):
        Page() | 42


def test_pdf_or_rejects_unsupported_type() -> None:
    with pytest.raises(TypeError, match="str"):
        Pdf() | "not a page"


def test_image_from_bytes_composes() -> None:
    png = make_gray8_png(2, 2, 128)
    page = Page() | Image(png, at=(72, 700), width=20.0, height=20.0)
    data = (Pdf() | page).to_bytes()
    assert data.startswith(b"%PDF-")


def test_image_from_path_reads_file_at_lowering(tmp_path: Path) -> None:
    png = make_gray8_png(2, 2, 200)
    path = tmp_path / "dot.png"
    path.write_bytes(png)
    page = Page() | Image(str(path), at=(72, 700))
    data = (Pdf() | page).to_bytes()
    assert data.startswith(b"%PDF-")


def test_image_decode_error_is_pdf_error() -> None:
    page = Page() | Image(b"not an image", at=(0, 0))
    with pytest.raises(pdfboss.PdfError):
        (Pdf() | page).to_bytes()


def test_link_composes_into_page() -> None:
    page = Page() | Link(rect=(0, 0, 100, 20), url="https://example.com")
    data = (Pdf() | page).to_bytes()
    assert data.startswith(b"%PDF-")


def test_letterhead_draw_protocol_composes_and_extracts(tmp_path: Path) -> None:
    class Letterhead:
        def draw(self, canvas: Canvas) -> None:
            canvas.line(72, 806, 523, 806, width=0.5)
            canvas.text("ACME GmbH", at=(72, 812), font=Standard14.HELVETICA, size=8)

    page = Page(size="a4") | Letterhead() | Text("Body copy", at=(72, 700))
    data = (Pdf() | page).to_bytes()
    path = tmp_path / "letterhead.pdf"
    path.write_bytes(data)
    doc = pdfboss.Document(str(path))
    text = doc.extract_text()
    assert "ACME GmbH" in text
    assert "Body copy" in text


def test_draw_exception_propagates_untouched() -> None:
    class Bomb:
        def draw(self, canvas: Canvas) -> None:
            raise ValueError("boom")

    page = Page(size="a4") | Bomb()
    with pytest.raises(ValueError, match="boom"):
        (Pdf() | page).to_bytes()


def test_draw_non_none_return_is_ignored() -> None:
    class Noisy:
        def draw(self, canvas: Canvas) -> object:
            canvas.text("noisy", at=(72, 700))
            return "ignored"

    page = Page(size="a4") | Noisy()
    data = (Pdf() | page).to_bytes()
    assert data.startswith(b"%PDF-")


def test_canvas_is_unusable_after_draw_returns() -> None:
    class Escaping:
        def __init__(self) -> None:
            self.canvas: Canvas | None = None

        def draw(self, canvas: Canvas) -> None:
            self.canvas = canvas

    escaping = Escaping()
    page = Page(size="a4") | escaping
    (Pdf() | page).to_bytes()
    assert escaping.canvas is not None
    with pytest.raises(pdfboss.PdfError, match="canvas is no longer usable outside draw"):
        escaping.canvas.text("late", at=(0, 0))


def test_canvas_is_unusable_after_draw_raises() -> None:
    class EscapingBomb:
        def __init__(self) -> None:
            self.canvas: Canvas | None = None

        def draw(self, canvas: Canvas) -> None:
            self.canvas = canvas
            raise RuntimeError("boom")

    escaping = EscapingBomb()
    page = Page(size="a4") | escaping
    with pytest.raises(RuntimeError, match="boom"):
        (Pdf() | page).to_bytes()
    assert escaping.canvas is not None
    with pytest.raises(pdfboss.PdfError, match="canvas is no longer usable outside draw"):
        escaping.canvas.text("late", at=(0, 0))


def test_outline_duplicate_slot_raises() -> None:
    with pytest.raises(TypeError, match="already has Outline"):
        Pdf() | Outline(Bookmark("A", 0)) | Outline(Bookmark("B", 0))


def test_viewer_duplicate_slot_raises() -> None:
    with pytest.raises(TypeError, match="already has Viewer"):
        Pdf() | Viewer(mode="use-none") | Viewer(mode="use-outlines")


def test_full_vocabulary_compose_reopens_with_expected_text_and_pages(tmp_path: Path) -> None:
    first = Page(size="a4") | Text("Cover page", at=(72, 700))
    second = Page(size="a4") | Text("Appendix page", at=(72, 700))
    pdf = (
        Pdf()
        | first
        | second
        | Outline(
            Bookmark("Summary", 0, children=(Bookmark("Detail", 0),)),
            Bookmark("Appendix", 1),
        )
        | Attachment("raw-numbers.csv", b"a,b,c\n1,2,3\n", mime="text/csv", description="Source data")
        | PageLabel(0, style="roman-lower", prefix="p. ")
        | Viewer(layout="single-page", mode="use-outlines", open_to=0)
    )
    data = pdf.to_bytes()
    path = tmp_path / "full.pdf"
    path.write_bytes(data)
    doc = pdfboss.Document(str(path))
    assert doc.page_count == 2
    text = doc.extract_text()
    assert "Cover page" in text
    assert "Appendix page" in text


def test_full_vocabulary_compose_is_deterministic() -> None:
    def build() -> bytes:
        page = Page(size="a4") | Text("Stable", at=(72, 700))
        pdf = (
            Pdf()
            | page
            | Outline(Bookmark("Only", 0))
            | Attachment("data.bin", b"\x00\x01\x02")
            | PageLabel(0)
            | Viewer(mode="use-none")
        )
        return pdf.to_bytes()

    assert build() == build()


def test_multiple_attachments_and_page_labels_compose() -> None:
    page = Page(size="a4") | Text("x", at=(72, 700))
    pdf = (
        Pdf()
        | page
        | Attachment("a.txt", b"a")
        | Attachment("b.txt", b"b")
        | PageLabel(0, style="decimal")
    )
    data = pdf.to_bytes()
    assert data.startswith(b"%PDF-")


def test_paragraph_wraps_and_full_text_survives(tmp_path: Path) -> None:
    words = [f"word{i}" for i in range(20)]
    body = " ".join(words)
    page = Page(size="a4") | Paragraph(body, rect=(72, 400, 222, 700), size=10)
    data = (Pdf() | page).to_bytes()
    path = tmp_path / "wrap.pdf"
    path.write_bytes(data)
    doc = pdfboss.Document(str(path))
    text = doc.extract_text()
    for word in words:
        assert word in text
    lines_with_words = [line for line in text.splitlines() if "word" in line]
    assert len(lines_with_words) >= 2, "the narrow rect must force more than one line"


def test_paragraph_overflow_raises_pdf_error() -> None:
    body = " ".join(f"word{i}" for i in range(20))
    page = Page(size="a4") | Paragraph(body, rect=(72, 690, 400, 700), size=12)
    with pytest.raises(pdfboss.PdfError, match="overflows"):
        (Pdf() | page).to_bytes()


def test_paragraph_rejects_unknown_align() -> None:
    with pytest.raises(TypeError, match="left, center, right"):
        Paragraph("x", rect=(0, 0, 10, 10), align="diagonal")


def test_page_label_rejects_unknown_style() -> None:
    with pytest.raises(TypeError, match="decimal"):
        PageLabel(0, style="weird")


def test_viewer_rejects_unknown_layout() -> None:
    with pytest.raises(TypeError, match="single-page"):
        Viewer(layout="whatever")


def test_viewer_rejects_unknown_mode() -> None:
    with pytest.raises(TypeError, match="use-none"):
        Viewer(mode="whatever")


def test_write_module_exports_full_vocabulary_sorted() -> None:
    from pdfboss.write import Pdf, Page, Text, Standard14  # noqa: F401

    assert pdfboss.write.__all__ == sorted(pdfboss.write.__all__)
    assert pdfboss.write.__all__ == [
        "Attachment",
        "Bookmark",
        "Canvas",
        "Image",
        "Link",
        "Metadata",
        "Outline",
        "Page",
        "PageLabel",
        "Paragraph",
        "Pdf",
        "Standard14",
        "Text",
        "Viewer",
        "watermark",
    ]


def test_standard14_exposes_all_fourteen_names() -> None:
    names = [
        "HELVETICA",
        "HELVETICA_BOLD",
        "HELVETICA_OBLIQUE",
        "HELVETICA_BOLD_OBLIQUE",
        "TIMES_ROMAN",
        "TIMES_BOLD",
        "TIMES_ITALIC",
        "TIMES_BOLD_ITALIC",
        "COURIER",
        "COURIER_BOLD",
        "COURIER_OBLIQUE",
        "COURIER_BOLD_OBLIQUE",
        "SYMBOL",
        "ZAPF_DINGBATS",
    ]
    for name in names:
        assert hasattr(Standard14, name)
