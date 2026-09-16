"""Tests for image placement: ``Page.images`` and its async twin.

Where each image a page draws sits, read from the content stream's
transformation state without decoding a pixel. Runs against the committed
fixture PDFs plus small in-memory documents built here.
"""

from pathlib import Path

import pytest

from pdfboss import AsyncDocument, Document, PlacedImage
from test_spans import build_pdf, stream

GRAY_2X2 = stream(
    b"/Type /XObject /Subtype /Image /Width 2 /Height 2 "
    b"/ColorSpace /DeviceGray /BitsPerComponent 8",
    bytes([0, 85, 170, 255]),
)
STENCIL_4X4 = stream(
    b"/Type /XObject /Subtype /Image /Width 4 /Height 4 "
    b"/ImageMask true /BitsPerComponent 1",
    bytes([0xF0, 0x0F, 0xF0, 0x0F]),
)
HELVETICA = (
    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica "
    b"/Encoding /WinAnsiEncoding >>"
)


def image_doc(
    content: bytes,
    xobjects: bytes = b"/Im1 5 0 R /St 7 0 R",
    extra: dict[int, bytes] | None = None,
    catalog: bytes = b"<< /Type /Catalog /Pages 2 0 R >>",
    properties: bytes = b"",
) -> bytes:
    """One 200 x 100 page drawing ``content`` with ``/Im1`` a 2 x 2 gray
    image, ``/St`` a 4 x 4 stencil mask and ``/F1`` Helvetica."""
    objects = {
        1: catalog,
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /XObject << " + xobjects + b" >> "
            b"/Font << /F1 6 0 R >> " + properties + b">> /Contents 4 0 R >>"
        ),
        4: stream(b"", content),
        5: GRAY_2X2,
        6: HELVETICA,
        7: STENCIL_4X4,
    }
    objects.update(extra or {})
    return build_pdf(objects)


def boxes(images: list[PlacedImage]) -> list[tuple[float, float, float, float]]:
    return [tuple(round(v, 3) for v in image.bbox) for image in images]


class TestPlacement:
    def test_the_fixture_image_sits_where_its_cm_puts_it(self, image_pdf: bytes) -> None:
        images = Document(data=image_pdf)[0].images()
        assert len(images) == 1
        image = images[0]
        assert boxes(images) == [(10.0, 10.0, 60.0, 60.0)]
        assert (image.width, image.height) == (2, 2)
        assert image.page == 0
        assert image.stencil is False
        assert image.inline is False
        assert repr(image).startswith("PlacedImage(page=0, bbox=(10")

    def test_a_page_without_images_places_nothing(self, hello_pdf: Path) -> None:
        assert Document(str(hello_pdf))[0].images() == []

    def test_each_draw_is_one_record_in_drawing_order(self) -> None:
        doc = image_doc(b"q 50 0 0 50 0 0 cm /Im1 Do Q q 20 0 0 10 100 50 cm /Im1 Do Q")
        assert boxes(Document(data=doc)[0].images()) == [
            (0.0, 0.0, 50.0, 50.0),
            (100.0, 50.0, 120.0, 60.0),
        ]

    def test_a_rotated_image_reports_the_box_around_its_outline(self) -> None:
        doc = image_doc(b"q 0 50 -50 0 60 10 cm /Im1 Do Q")
        assert boxes(Document(data=doc)[0].images()) == [(10.0, 10.0, 60.0, 60.0)]

    def test_a_form_matrix_composes_with_the_callers_ctm(self) -> None:
        form = stream(
            b"/Type /XObject /Subtype /Form /Matrix [2 0 0 2 0 0] "
            b"/Resources << /XObject << /Im1 5 0 R >> >>",
            b"q 10 0 0 10 5 5 cm /Im1 Do Q",
        )
        doc = image_doc(
            b"q 1 0 0 1 100 0 cm /Fx Do Q",
            xobjects=b"/Im1 5 0 R /Fx 8 0 R",
            extra={8: form},
        )
        assert boxes(Document(data=doc)[0].images()) == [(110.0, 10.0, 130.0, 30.0)]

    def test_a_stencil_mask_is_placed_but_not_extracted(self) -> None:
        doc = image_doc(b"q 20 0 0 20 0 0 cm /St Do Q")
        page = Document(data=doc)[0]
        images = page.images()
        assert boxes(images) == [(0.0, 0.0, 20.0, 20.0)]
        assert images[0].stencil is True
        assert (images[0].width, images[0].height) == (4, 4)
        assert page.extract_images() == []

    def test_an_inline_image_is_placed(self) -> None:
        doc = image_doc(b"q 30 0 0 20 10 70 cm BI /W 2 /H 1 /BPC 8 /CS /G /F /AHx ID 00FF> EI Q")
        images = Document(data=doc)[0].images()
        assert boxes(images) == [(10.0, 70.0, 40.0, 90.0)]
        assert images[0].inline is True
        assert (images[0].width, images[0].height) == (2, 1)

    def test_an_xobject_of_another_subtype_places_nothing(self) -> None:
        postscript = stream(b"/Type /XObject /Subtype /PS", b"0 0 moveto")
        doc = image_doc(b"/Ps Do", xobjects=b"/Ps 8 0 R", extra={8: postscript})
        assert Document(data=doc)[0].images() == []

    def test_hidden_optional_content_is_excluded(self) -> None:
        gated = stream(
            b"/Type /XObject /Subtype /Image /Width 2 /Height 2 "
            b"/ColorSpace /DeviceGray /BitsPerComponent 8 /OC 9 0 R",
            bytes([0, 85, 170, 255]),
        )
        doc = image_doc(
            b"/OC /H BDC q 50 0 0 50 0 0 cm /Im1 Do Q EMC "
            b"q 30 0 0 30 100 0 cm /Gated Do Q "
            b"q 10 0 0 10 0 0 cm /Im1 Do Q",
            xobjects=b"/Im1 5 0 R /Gated 10 0 R",
            extra={9: b"<< /Type /OCG /Name (hidden) >>", 10: gated},
            catalog=(
                b"<< /Type /Catalog /Pages 2 0 R "
                b"/OCProperties << /OCGs [9 0 R] /D << /OFF [9 0 R] >> >> >>"
            ),
            properties=b"/Properties << /H 9 0 R >> ",
        )
        assert boxes(Document(data=doc)[0].images()) == [(0.0, 0.0, 10.0, 10.0)]

    def test_spans_and_images_share_one_frame(self) -> None:
        doc = image_doc(
            b"q 1 0 0 1 100 0 cm BT /F1 12 Tf 0 20 Td (X) Tj ET "
            b"30 0 0 30 0 0 cm /Im1 Do Q"
        )
        page = Document(data=doc)[0]
        [span] = page.spans()
        [image] = page.images()
        assert round(span.x, 3) == 100.0
        assert round(image.bbox[0], 3) == 100.0


class TestAsyncParity:
    @pytest.mark.asyncio
    async def test_async_images_match_sync(self, image_pdf: bytes) -> None:
        doc = await AsyncDocument.from_bytes(image_pdf)
        images = await doc[0].images()
        sync_images = Document(data=image_pdf)[0].images()
        assert boxes(images) == boxes(sync_images) == [(10.0, 10.0, 60.0, 60.0)]
        assert [(i.width, i.height, i.stencil, i.inline) for i in images] == [
            (2, 2, False, False)
        ]
