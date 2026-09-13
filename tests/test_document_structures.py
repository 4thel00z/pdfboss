"""The document- and page-level structure readers: the linearization
parameter dictionary, output intents, page pieces, thumbnails, article
threads, presentations and permission handlers, sync and async, plus the
variable text entries of form fields.

Every fixture is a PDF built inline; the object bodies follow the clause
examples the core tests use.
"""

import asyncio
from pathlib import Path

import pytest

from pdfboss import (
    ArticleThread,
    AsyncDocument,
    Bead,
    DefaultAppearance,
    Document,
    Linearization,
    OutputIntent,
    PageImage,
    PagePiece,
    PermissionHandlers,
    Presentation,
    Signature,
    Thumbnail,
    Transition,
)
from test_forms import widget
from test_spans import build_pdf, stream

RGB_2X1 = bytes([255, 0, 0, 0, 0, 255])
LENGTH_PLACEHOLDER = b"0000000000"


def linearized_file(length: bytes) -> bytes:
    """A one-page file whose first object is the linearization parameter
    dictionary declaring ``length`` as the file length; the catalog is
    object 44, so the objects are written in the order given."""
    objects = [
        (43, b"<< /Linearized 1 /L " + length + b" /H [ 0 0 ] /O 46 /E 0 /N 1 /T 0 >>"),
        (44, b"<< /Type /Catalog /Pages 45 0 R >>"),
        (45, b"<< /Type /Pages /Kids [46 0 R] /Count 1 >>"),
        (46, b"<< /Type /Page /Parent 45 0 R /MediaBox [0 0 10 10] >>"),
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = {}
    for num, body in objects:
        offsets[num] = len(out)
        out += b"%d 0 obj\n%s\nendobj\n" % (num, body)
    xref_at = len(out)
    out += b"xref\n0 1\n0000000000 65535 f \n43 4\n"
    for num, _ in objects:
        out += b"%010d 00000 n \n" % offsets[num]
    out += b"trailer\n<< /Size 47 /Root 44 0 R >>\nstartxref\n%d\n%%%%EOF\n" % xref_at
    return bytes(out)


@pytest.fixture
def linearized_pdf() -> bytes:
    """The linearized file with ``/L`` patched to its actual length; the
    placeholder keeps the width, so the length does not move."""
    data = linearized_file(LENGTH_PLACEHOLDER)
    return data.replace(LENGTH_PLACEHOLDER, b"%010d" % len(data), 1)


@pytest.fixture
def structures_pdf() -> bytes:
    """Two pages. The catalog carries two output intents (one by
    reference), page pieces for two products, one article thread of three
    beads and both permission handlers, every object numbered in one run
    because the builder writes a contiguous xref table; the first page carries a 2x1
    thumbnail, a page piece, a five-second display with a split transition
    and the thread's first two beads; the second page only a fly
    transition."""
    return build_pdf(
        {
            1: (
                b"<< /Type /Catalog /Pages 2 0 R "
                b"/OutputIntents [5 0 R << /S /GTS_PDFA1 /OutputConditionIdentifier (sRGB IEC61966-2.1) "
                b"/Info <FEFF007300520047004200200070007200650076006900650077> >>] "
                b"/PieceInfo << /Photoshop 7 0 R "
                b"/Illustrator << /LastModified (D:20240102030405Z) /Private << /Version 28 >> >> >> "
                b"/Threads [9 0 R] "
                b"/Perms << /DocMDP 8 0 R /UR3 << /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.sha1 "
                b"/ByteRange [0 1 2 3] /Contents <ff> /Reason (Usage rights) >> >> >>"
            ),
            2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Thumb 6 0 R "
                b"/PieceInfo << /Illustrator << /LastModified (D:20240102030405Z) >> >> "
                b"/Dur 5 /Trans << /Type /Trans /D 3.5 /S /Split /Dm /V /M /O >> "
                b"/B [10 0 R 11 0 R] >>"
            ),
            4: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
                b"/Trans << /S /Fly /Di /None /SS 0.5 /B true >> >>"
            ),
            5: (
                b"<< /Type /OutputIntent /S /GTS_PDFX /OutputCondition (CGATS TR 001 (SWOP)) "
                b"/OutputConditionIdentifier (CGATS TR 001) /RegistryName (http://www.color.org) "
                b"/DestOutputProfile 100 0 R >>"
            ),
            6: stream(b"/Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8", RGB_2X1),
            7: b"<< /LastModified (D:20230601120000Z) /Private (opaque) >>",
            8: (
                b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached "
                b"/ByteRange [0 10 20 30] /Contents <0102> /Name (Certifier) >>"
            ),
            9: b"<< /F 10 0 R /I << /Title (Man Bites Dog) /Author (Ann) >> >>",
            10: b"<< /T 9 0 R /N 11 0 R /V 12 0 R /P 3 0 R /R [158 247 318 905] >>",
            11: b"<< /N 12 0 R /V 10 0 R /P 3 0 R /R [322 246 486 904] >>",
            12: b"<< /N 10 0 R /V 11 0 R /P 3 0 R /R [1 2 3 4] >>",
        }
    )


@pytest.fixture
def variable_text_pdf() -> bytes:
    """One page with a form whose document-wide ``/DA`` and ``/Q`` apply to
    a field that sets neither, and a second field that sets its own
    ``/DA``, ``/Q``, ``/DS`` and ``/RV``, the rich text value as a
    stream."""
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [5 0 R 6 0 R] >>",
            4: b"<< /Fields [5 0 R 6 0 R] /DA (/Helv 0 Tf 0 g) /Q 1 >>",
            5: widget(b"/T (plain) /FT /Tx"),
            6: widget(
                b"/T (styled) /FT /Tx /DA (/TiRo 12 Tf 1 0 0 rg) /Q 2 "
                b"/DS (font: 12pt Times) /RV 7 0 R"
            ),
            7: stream(b"", b"<p>Hello</p>"),
        }
    )


class TestLinearization:
    def test_reads_every_entry_of_the_parameter_dictionary(self, linearized_pdf: bytes) -> None:
        doc = Document(data=linearized_pdf)
        record = doc.linearization()
        assert isinstance(record, Linearization)
        assert record.version == 1.0
        assert record.file_length == len(linearized_pdf)
        assert record.hint_streams == [(0, 0)]
        assert record.first_page_object == 46
        assert record.first_page_end == 0
        assert record.page_count == 1
        assert record.main_xref_offset == 0
        assert record.first_page == 0
        assert doc.is_linearized() is True
        assert repr(record) == (
            f"Linearization(version=1.0, file_length={len(linearized_pdf)}, page_count=1)"
        )

    def test_a_stale_length_makes_the_file_ordinary(self) -> None:
        doc = Document(data=linearized_file(b"0000000001"))
        record = doc.linearization()
        assert record is not None
        assert record.file_length == 1
        assert doc.is_linearized() is False

    def test_a_file_without_the_dictionary(self, hello_pdf: Path) -> None:
        doc = Document(hello_pdf)
        assert doc.linearization() is None
        assert doc.is_linearized() is False


class TestOutputIntents:
    def test_reads_each_intent_in_array_order(self, structures_pdf: bytes) -> None:
        intents = Document(data=structures_pdf).output_intents()
        assert [intent.subtype for intent in intents] == ["GTS_PDFX", "GTS_PDFA1"]
        pdfx, pdfa = intents
        assert isinstance(pdfx, OutputIntent)
        assert pdfx.output_condition == "CGATS TR 001 (SWOP)"
        assert pdfx.output_condition_identifier == "CGATS TR 001"
        assert pdfx.registry_name == "http://www.color.org"
        assert pdfx.info is None
        assert pdfx.destination_profile == (100, 0)
        assert pdfa.output_condition is None
        assert pdfa.output_condition_identifier == "sRGB IEC61966-2.1"
        assert pdfa.registry_name is None
        assert pdfa.info == "sRGB preview"
        assert pdfa.destination_profile is None
        assert repr(pdfx) == (
            "OutputIntent(subtype='GTS_PDFX', output_condition_identifier='CGATS TR 001')"
        )

    def test_a_document_without_intents(self, hello_pdf: Path) -> None:
        assert Document(hello_pdf).output_intents() == []


class TestPieceInfo:
    def test_document_pieces_come_sorted_by_product(self, structures_pdf: bytes) -> None:
        pieces = Document(data=structures_pdf).piece_info()
        assert [piece.product for piece in pieces] == ["Illustrator", "Photoshop"]
        illustrator, photoshop = pieces
        assert isinstance(illustrator, PagePiece)
        assert illustrator.last_modified == "2024-01-02T03:04:05Z"
        assert illustrator.private == {"Version": 28}
        assert photoshop.last_modified == "2023-06-01T12:00:00Z"
        assert photoshop.private == "opaque"
        assert repr(illustrator) == (
            "PagePiece(product='Illustrator', last_modified='2024-01-02T03:04:05Z')"
        )

    def test_page_pieces_come_from_the_page(self, structures_pdf: bytes) -> None:
        doc = Document(data=structures_pdf)
        first = doc[0].piece_info()
        assert [piece.product for piece in first] == ["Illustrator"]
        assert first[0].private is None
        assert doc[1].piece_info() == []

    def test_a_document_without_pieces(self, hello_pdf: Path) -> None:
        doc = Document(hello_pdf)
        assert doc.piece_info() == []
        assert doc[0].piece_info() == []


class TestThumbnail:
    def test_reads_the_entries_and_decodes_the_image(self, structures_pdf: bytes) -> None:
        doc = Document(data=structures_pdf)
        thumb = doc[0].thumbnail()
        assert isinstance(thumb, Thumbnail)
        assert (thumb.width, thumb.height) == (2, 1)
        assert thumb.bits_per_component == 8
        assert thumb.color_space == "DeviceRGB"
        assert thumb.decode is None
        assert repr(thumb) == "Thumbnail(width=2, height=1)"
        image = doc[0].thumbnail_image()
        assert isinstance(image, PageImage)
        assert (image.width, image.height) == (2, 1)
        assert image.data.startswith(b"\x89PNG")

    def test_a_page_without_a_thumbnail(self, structures_pdf: bytes) -> None:
        page = Document(data=structures_pdf)[1]
        assert page.thumbnail() is None
        assert page.thumbnail_image() is None


class TestArticles:
    def test_walks_each_threads_beads_in_order(self, structures_pdf: bytes) -> None:
        doc = Document(data=structures_pdf)
        threads = doc.articles()
        assert len(threads) == 1
        thread = threads[0]
        assert isinstance(thread, ArticleThread)
        assert thread.ref == (9, 0)
        assert thread.info == {"title": "Man Bites Dog", "author": "Ann"}
        assert [bead.ref for bead in thread.beads] == [(10, 0), (11, 0), (12, 0)]
        first = thread.beads[0]
        assert isinstance(first, Bead)
        assert first.page_ref == (3, 0)
        assert first.page == 0
        assert first.rect == (158.0, 247.0, 318.0, 905.0)
        assert thread.beads[2].rect == (1.0, 2.0, 3.0, 4.0)
        assert repr(thread) == "ArticleThread(title='Man Bites Dog', beads=3)"
        assert repr(first) == "Bead(ref=(10, 0), page=0)"

    def test_a_page_lists_the_beads_on_it(self, structures_pdf: bytes) -> None:
        doc = Document(data=structures_pdf)
        assert doc[0].beads() == [(10, 0), (11, 0)]
        assert doc[1].beads() == []

    def test_a_document_without_threads(self, hello_pdf: Path) -> None:
        doc = Document(hello_pdf)
        assert doc.articles() == []
        assert doc[0].beads() == []


class TestPresentation:
    def test_reads_the_duration_and_the_transition_with_defaults(
        self, structures_pdf: bytes
    ) -> None:
        doc = Document(data=structures_pdf)
        shown = doc[0].presentation()
        assert isinstance(shown, Presentation)
        assert shown.duration == 5.0
        transition = shown.transition
        assert isinstance(transition, Transition)
        assert transition.style == "split"
        assert transition.duration == 3.5
        assert transition.dimension == "vertical"
        assert transition.motion == "outward"
        assert transition.direction == 0
        assert transition.scale == 1.0
        assert transition.opaque is False
        assert repr(shown) == "Presentation(duration=5.0, transition='split')"
        assert repr(transition) == "Transition(style='split', duration=3.5)"

    def test_a_fly_transition_without_a_duration(self, structures_pdf: bytes) -> None:
        fly = Document(data=structures_pdf)[1].presentation()
        assert fly is not None
        assert fly.duration is None
        assert fly.transition is not None
        assert fly.transition.style == "fly"
        assert fly.transition.direction is None
        assert fly.transition.scale == 0.5
        assert fly.transition.opaque is True
        assert repr(fly) == "Presentation(duration=None, transition='fly')"

    def test_a_page_without_a_presentation(self, hello_pdf: Path) -> None:
        assert Document(hello_pdf)[0].presentation() is None


class TestPermissionHandlers:
    def test_reads_both_handlers_as_signatures(self, structures_pdf: bytes) -> None:
        handlers = Document(data=structures_pdf).permission_handlers()
        assert isinstance(handlers, PermissionHandlers)
        doc_mdp = handlers.doc_mdp
        assert isinstance(doc_mdp, Signature)
        assert doc_mdp.filter == "Adobe.PPKLite"
        assert doc_mdp.sub_filter == "adbe.pkcs7.detached"
        assert doc_mdp.byte_range == [(0, 10), (20, 30)]
        assert doc_mdp.contents == b"\x01\x02"
        assert doc_mdp.name == "Certifier"
        usage_rights = handlers.usage_rights
        assert usage_rights is not None
        assert usage_rights.sub_filter == "adbe.pkcs7.sha1"
        assert usage_rights.reason == "Usage rights"
        assert usage_rights.name is None
        assert repr(handlers) == "PermissionHandlers(doc_mdp=True, usage_rights=True)"

    def test_a_document_without_handlers(self, hello_pdf: Path) -> None:
        assert Document(hello_pdf).permission_handlers() is None


class TestVariableText:
    def test_a_field_inherits_the_forms_defaults(self, variable_text_pdf: bytes) -> None:
        fields = {field.name: field for field in Document(data=variable_text_pdf).form_fields()}
        plain = fields["plain"]
        assert plain.default_appearance == "/Helv 0 Tf 0 g"
        assert plain.quadding == "centered"
        assert plain.default_style is None
        assert plain.rich_text is None

    def test_a_field_sets_its_own_entries(self, variable_text_pdf: bytes) -> None:
        fields = {field.name: field for field in Document(data=variable_text_pdf).form_fields()}
        styled = fields["styled"]
        assert styled.default_appearance == "/TiRo 12 Tf 1 0 0 rg"
        assert styled.quadding == "right"
        assert styled.default_style == "font: 12pt Times"
        assert styled.rich_text == "<p>Hello</p>"

    def test_parses_a_default_appearance_string(self) -> None:
        appearance = DefaultAppearance.parse("/TiRo 12 Tf 1 0 0 rg")
        assert isinstance(appearance, DefaultAppearance)
        assert appearance.font == "TiRo"
        assert appearance.font_size == 12.0
        assert appearance.fill_color == [1.0, 0.0, 0.0]
        assert repr(appearance) == (
            "DefaultAppearance(font='TiRo', font_size=12.0, fill_color=[1.0, 0.0, 0.0])"
        )
        gray = DefaultAppearance.parse("/Helv 0 Tf 0 g")
        assert (gray.font, gray.font_size, gray.fill_color) == ("Helv", 0.0, [0.0])
        empty = DefaultAppearance.parse("")
        assert (empty.font, empty.font_size, empty.fill_color) == (None, None, None)
        assert repr(empty) == "DefaultAppearance(font=None, font_size=None, fill_color=None)"


class TestAsyncTwins:
    def test_every_reader_has_a_twin(self, structures_pdf: bytes, linearized_pdf: bytes) -> None:
        async def run() -> None:
            doc = await AsyncDocument.from_bytes(structures_pdf)
            assert doc.linearization() is None
            assert doc.is_linearized() is False
            intents = await doc.output_intents()
            assert [intent.subtype for intent in intents] == ["GTS_PDFX", "GTS_PDFA1"]
            pieces = await doc.piece_info()
            assert [piece.product for piece in pieces] == ["Illustrator", "Photoshop"]
            threads = await doc.articles()
            assert [bead.ref for bead in threads[0].beads] == [(10, 0), (11, 0), (12, 0)]
            assert threads[0].beads[0].page == 0
            handlers = await doc.permission_handlers()
            assert handlers is not None
            assert handlers.doc_mdp is not None
            assert handlers.doc_mdp.name == "Certifier"
            page = doc[0]
            assert [piece.product for piece in await page.piece_info()] == ["Illustrator"]
            thumb = await page.thumbnail()
            assert thumb is not None
            assert (thumb.width, thumb.height) == (2, 1)
            image = await page.thumbnail_image()
            assert image is not None
            assert (image.width, image.height) == (2, 1)
            assert await page.beads() == [(10, 0), (11, 0)]
            shown = await page.presentation()
            assert shown is not None
            assert shown.transition is not None
            assert shown.transition.style == "split"
            assert await doc[1].thumbnail() is None
            assert await doc[1].thumbnail_image() is None
            linear = await AsyncDocument.from_bytes(linearized_pdf)
            assert linear.is_linearized() is True
            record = linear.linearization()
            assert record is not None
            assert record.page_count == 1

        asyncio.run(run())
