"""Shared fixtures for the pdfboss pytest suite."""

from pathlib import Path

import pytest

FIXTURES = Path(__file__).parent / "fixtures"


@pytest.fixture
def fixtures_dir() -> Path:
    """Directory containing the committed fixture PDFs."""
    return FIXTURES


@pytest.fixture
def hello_pdf(fixtures_dir: Path) -> Path:
    return fixtures_dir / "hello.pdf"


@pytest.fixture
def three_pages_pdf(fixtures_dir: Path) -> Path:
    return fixtures_dir / "three-pages.pdf"


@pytest.fixture
def shapes_pdf(fixtures_dir: Path) -> Path:
    return fixtures_dir / "shapes.pdf"


@pytest.fixture
def xref_stream_pdf(fixtures_dir: Path) -> Path:
    return fixtures_dir / "xref-stream.pdf"


@pytest.fixture
def image_pdf() -> bytes:
    """A one-page PDF drawing a 2x2 DeviceRGB image XObject whose /SMask
    fades from transparent to opaque, built in memory with a correct
    classic xref table."""
    rgb = bytes([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0])
    gray = bytes([0, 85, 170, 255])
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
        ),
        4: b"<< /Length 30 >>\nstream\nq 50 0 0 50 10 10 cm /Im1 Do Q\nendstream",
        5: (
            b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 "
            b"/ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R "
            b"/Length 12 >>\nstream\n" + rgb + b"\nendstream"
        ),
        6: (
            b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 "
            b"/ColorSpace /DeviceGray /BitsPerComponent 8 "
            b"/Length 4 >>\nstream\n" + gray + b"\nendstream"
        ),
    }
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


@pytest.fixture
def boxed_pdf() -> bytes:
    """A one-page PDF declaring /CropBox, /BleedBox and /TrimBox but no
    /ArtBox, built in memory with a correct classic xref table."""
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] "
            b"/CropBox [50 50 550 750] /BleedBox [40 40 560 760] "
            b"/TrimBox [60 70 540 730] >>"
        ),
    }
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
