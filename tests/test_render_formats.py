"""Output formats on the render entry points: PPM and BMP next to PNG.

Both new formats are parsed here with the stdlib only, then compared pixel
for pixel with the PNG of the same render, so the tests always run.
"""

from pathlib import Path

import pytest

from pdfboss import AsyncDocument, Document
from test_pdfboss import decode_png

STUB = Path(__file__).parent.parent / "python" / "pdfboss" / "_pdfboss.pyi"


def parse_ppm(data: bytes) -> tuple[int, int, bytes]:
    magic, size, maxval, pixels = data.split(b"\n", 3)
    assert magic == b"P6"
    assert maxval == b"255"
    width, height = (int(v) for v in size.split())
    assert len(pixels) == width * height * 3
    return width, height, pixels


def parse_bmp(data: bytes) -> tuple[int, int, bytes]:
    """Top-down RGB rows read back from a 24-bit bottom-up BMP."""
    assert data[:2] == b"BM"
    offset = int.from_bytes(data[10:14], "little")
    width = int.from_bytes(data[18:22], "little", signed=True)
    height = int.from_bytes(data[22:26], "little", signed=True)
    assert int.from_bytes(data[28:30], "little") == 24
    stride = (width * 3 + 3) & ~3
    rgb = bytearray()
    for y in reversed(range(height)):
        row = data[offset + y * stride : offset + y * stride + width * 3]
        for x in range(width):
            b, g, r = row[x * 3 : x * 3 + 3]
            rgb += bytes((r, g, b))
    return width, height, bytes(rgb)


def rgb_of(rgba: bytes) -> bytes:
    return bytes(v for i, v in enumerate(rgba) if i % 4 != 3)


def test_ppm_carries_the_png_pixels_without_alpha(hello_pdf: Path) -> None:
    page = Document(hello_pdf)[0]
    width, height, rgba = decode_png(page.render(scale=0.5))
    assert parse_ppm(page.render(scale=0.5, format="ppm")) == (width, height, rgb_of(rgba))


def test_bmp_carries_the_png_pixels_without_alpha(hello_pdf: Path) -> None:
    page = Document(hello_pdf)[0]
    width, height, rgba = decode_png(page.render(scale=0.5))
    assert parse_bmp(page.render(scale=0.5, format="bmp")) == (width, height, rgb_of(rgba))


def test_png_stays_the_default_format(hello_pdf: Path) -> None:
    page = Document(hello_pdf)[0]
    assert page.render(scale=0.5, format="png") == page.render(scale=0.5)


def test_unknown_format_raises_value_error(hello_pdf: Path) -> None:
    with pytest.raises(ValueError, match="tiff"):
        Document(hello_pdf)[0].render(format="tiff")


def test_render_reporting_and_render_pages_take_the_format(hello_pdf: Path) -> None:
    doc = Document(hello_pdf)
    bmp, warnings = doc[0].render_reporting(scale=0.5, format="bmp")
    assert bmp[:2] == b"BM"
    assert warnings == []
    assert [p[:2] for p in doc.render_pages(scale=0.5, format="ppm")] == [b"P6"]
    assert doc.render_pages(pages=[0], scale=0.5, format="bmp") == [bmp]


@pytest.mark.asyncio
async def test_async_twins_take_the_format(hello_pdf: Path) -> None:
    doc = await AsyncDocument.open(hello_pdf)
    page = doc[0]
    ppm = await page.render(scale=0.5, format="ppm")
    assert parse_ppm(ppm)[:2] == parse_ppm(Document(hello_pdf)[0].render(scale=0.5, format="ppm"))[:2]
    bmp, warnings = await page.render_reporting(scale=0.5, format="bmp")
    assert bmp[:2] == b"BM"
    assert warnings == []
    assert await doc.render_pages(pages=[0], scale=0.5, format="bmp") == [bmp]


def test_stub_declares_the_format_on_every_render_entry_point() -> None:
    assert STUB.read_text().count('format: str = "png"') == 6


def test_jpeg_is_a_jfif_stream_and_jpg_is_the_same_format(hello_pdf: Path) -> None:
    page = Document(hello_pdf)[0]
    jpeg = page.render(scale=0.5, format="jpeg")
    assert jpeg[:4] == b"\xff\xd8\xff\xe0"
    assert jpeg[6:11] == b"JFIF\0"
    assert jpeg[-2:] == b"\xff\xd9"
    assert page.render(scale=0.5, format="jpg") == jpeg


def test_jpeg_quality_orders_file_size(hello_pdf: Path) -> None:
    page = Document(hello_pdf)[0]
    sizes = [len(page.render(scale=0.5, format="jpeg", quality=q)) for q in (20, 60, 95)]
    assert sizes == sorted(sizes) and len(set(sizes)) == 3, sizes
    assert page.render(scale=0.5, format="jpeg") == page.render(scale=0.5, format="jpeg", quality=90)


def test_jpeg_quality_outside_one_to_hundred_raises(hello_pdf: Path) -> None:
    page = Document(hello_pdf)[0]
    for quality in (0, 101):
        with pytest.raises(ValueError, match="quality"):
            page.render(scale=0.5, format="jpeg", quality=quality)


@pytest.mark.asyncio
async def test_async_render_takes_jpeg_quality(hello_pdf: Path) -> None:
    doc = await AsyncDocument.open(hello_pdf)
    small = await doc[0].render(scale=0.5, format="jpeg", quality=30)
    large = await doc[0].render(scale=0.5, format="jpeg", quality=95)
    assert small[:2] == b"\xff\xd8"
    assert len(small) < len(large)


def test_stub_declares_the_jpeg_quality_on_every_render_entry_point() -> None:
    assert STUB.read_text().count("quality: int = 90") == 6
