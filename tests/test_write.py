"""Tests for `pdfboss.write`: the composition core (Task 9) — frozen
pyclasses accumulated with `|`, lowered once into a Rust `Pdf` at
`save`/`to_bytes` time."""

import struct
import zlib
from pathlib import Path

import pytest

import pdfboss
from pdfboss.write import Image, Link, Metadata, Page, Pdf, Standard14, Text


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
