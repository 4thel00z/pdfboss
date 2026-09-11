"""The catalog readers: outline, named destinations, page labels, embedded
files, viewer preferences and developer extensions, sync and async.

The outline, page labels and one attachment round-trip through
``pdfboss.write``; the viewer preferences, extensions, named destinations
and a dated attachment come from a PDF built inline.
"""

import asyncio
from pathlib import Path

import pytest

from pdfboss import (
    AsyncDocument,
    Destination,
    Document,
    EmbeddedFile,
    OutlineItem,
    ViewerPreferences,
)
from pdfboss.write import Attachment, Bookmark, Outline, Page, PageLabel, Pdf
from test_spans import build_pdf, stream

CSV = b"a,b\n1,2\n"


@pytest.fixture
def written_pdf() -> bytes:
    """Three pages with a two-level outline, two label ranges and one
    attachment, all written by ``pdfboss.write``."""
    pdf = (
        Pdf()
        | Page(size="a4")
        | Page(size="a4")
        | Page(size="a4")
        | Outline(
            Bookmark("Chapter 1", 0, children=[Bookmark("Section 1.1", 1)]),
            Bookmark("Chapter 2", 2),
        )
        | Attachment("notes.txt", b"hello attachment", mime="text/plain", description="Notes")
        | PageLabel(0, style="roman-lower")
        | PageLabel(2, style="decimal", prefix="A-", start_at=5)
    )
    return pdf.to_bytes()


@pytest.fixture
def catalog_pdf() -> bytes:
    """Two pages; the catalog declares viewer preferences, one developer
    extension, a named destination in its ``/Dests`` dictionary and one
    in its name tree, and an embedded file with dates and a checksum."""
    return build_pdf(
        {
            1: (
                b"<< /Type /Catalog /Pages 2 0 R "
                b"/ViewerPreferences << /HideToolbar true /Direction /R2L /PrintScaling /None "
                b"/Duplex /DuplexFlipLongEdge /PrintPageRange [1 2 3 3] /NumCopies 2 "
                b"/ViewArea /TrimBox /PickTrayByPDFSize false >> "
                b"/Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 3 >> >> "
                b"/Dests << /intro [3 0 R /XYZ 10 700 1.5] >> "
                b"/Names << /Dests << /Names [(summary) [4 0 R /FitR 0 0 100 200]] >> "
                b"/EmbeddedFiles << /Names [(report.csv) 5 0 R] >> >> >>"
            ),
            2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
            4: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
            5: (
                b"<< /Type /Filespec /F (report.csv) /UF (report.csv) /Desc (Quarterly) "
                b"/EF << /F 6 0 R >> >>"
            ),
            6: stream(
                b"/Type /EmbeddedFile /Subtype /text#2Fcsv /Params << /Size 8 "
                b"/CreationDate (D:20260901120000Z) /ModDate (D:20260902130000+02'00') "
                b"/CheckSum <00112233445566778899aabbccddeeff> >>",
                CSV,
            ),
        }
    )


def page_refs(doc: Document) -> list[tuple[int, int]]:
    return [e.ref for e in doc.elements(physical=False) if e.kind == "page"]


class TestOutline:
    def test_written_outline_reads_back_with_pages_resolved(self, written_pdf: bytes) -> None:
        doc = Document(data=written_pdf)
        outline = doc.outline()
        assert [item.title for item in outline] == ["Chapter 1", "Chapter 2"]
        chapter, second = outline
        assert isinstance(chapter, OutlineItem)
        assert chapter.page == 0
        assert [child.title for child in chapter.children] == ["Section 1.1"]
        assert chapter.children[0].page == 1
        assert chapter.children[0].children == []
        assert second.page == 2
        assert second.color == (0.0, 0.0, 0.0)
        assert second.bold is False
        assert second.italic is False
        assert second.structure_element is None
        assert repr(chapter) == "OutlineItem(title='Chapter 1', page=0, children=1)"

    def test_destination_names_the_page_object(self, written_pdf: bytes) -> None:
        doc = Document(data=written_pdf)
        destination = doc.outline()[1].destination
        assert isinstance(destination, Destination)
        assert destination.page == 2
        assert destination.page_ref == page_refs(doc)[2]
        assert destination.fit == "xyz"
        assert destination.right is None
        assert destination.bottom is None

    def test_a_document_without_one(self, hello_pdf: Path) -> None:
        assert Document(hello_pdf).outline() == []


class TestNamedDestinations:
    def test_dictionary_and_tree_entries_together(self, catalog_pdf: bytes) -> None:
        doc = Document(data=catalog_pdf)
        found = doc.named_destinations()
        assert sorted(found) == ["intro", "summary"]
        intro = found["intro"]
        assert intro.page == 0
        assert intro.page_ref == (3, 0)
        assert intro.fit == "xyz"
        assert (intro.left, intro.top, intro.zoom) == (10.0, 700.0, 1.5)
        assert repr(intro) == "Destination(page=0, fit='xyz')"
        summary = found["summary"]
        assert summary.page == 1
        assert summary.fit == "fit-r"
        assert (summary.left, summary.bottom, summary.right, summary.top) == (
            0.0,
            0.0,
            100.0,
            200.0,
        )
        assert summary.zoom is None

    def test_a_document_without_any(self, hello_pdf: Path) -> None:
        assert Document(hello_pdf).named_destinations() == {}


class TestPageLabels:
    def test_ranges_and_labels(self, written_pdf: bytes) -> None:
        doc = Document(data=written_pdf)
        ranges = doc.page_labels()
        assert ranges is not None
        first, second = ranges
        assert (first.first_page, first.style, first.prefix, first.start_at) == (
            0,
            "roman-lower",
            None,
            1,
        )
        assert (second.first_page, second.style, second.prefix, second.start_at) == (
            2,
            "decimal",
            "A-",
            5,
        )
        assert [doc.page_label(i) for i in range(3)] == ["i", "ii", "A-5"]
        assert doc.page_label(3) is None
        assert second.label(2) == "A-5"
        assert repr(first) == "PageLabel(first_page=0, style='roman-lower', prefix=None, start_at=1)"

    def test_a_document_without_labels(self, hello_pdf: Path) -> None:
        doc = Document(hello_pdf)
        assert doc.page_labels() is None
        assert doc.page_label(0) is None


class TestEmbeddedFiles:
    def test_written_attachment_reads_back(self, written_pdf: bytes) -> None:
        doc = Document(data=written_pdf)
        (attachment,) = doc.embedded_files()
        assert isinstance(attachment, EmbeddedFile)
        assert attachment.name == "notes.txt"
        assert attachment.file_name == "notes.txt"
        assert attachment.mime == "text/plain"
        assert attachment.description == "Notes"
        assert attachment.size == len(b"hello attachment")
        assert attachment.created is None
        assert attachment.ref is not None
        assert doc.embedded_file_data(attachment) == b"hello attachment"
        assert repr(attachment) == "EmbeddedFile(name='notes.txt', mime='text/plain', size=16)"

    def test_dates_checksum_and_file_system(self, catalog_pdf: bytes) -> None:
        doc = Document(data=catalog_pdf)
        (report,) = doc.embedded_files()
        assert report.name == "report.csv"
        assert report.description == "Quarterly"
        assert report.mime == "text/csv"
        assert report.size == 8
        assert report.created == "2026-09-01T12:00:00Z"
        assert report.modified == "2026-09-02T13:00:00+02:00"
        assert report.checksum == bytes.fromhex("00112233445566778899aabbccddeeff")
        assert report.file_system is None
        assert report.volatile is False
        assert report.ref == (6, 0)
        assert doc.embedded_file_data(report) == CSV

    def test_a_document_without_attachments(self, hello_pdf: Path) -> None:
        assert Document(hello_pdf).embedded_files() == []


class TestViewerPreferences:
    def test_every_entry_and_its_default(self, catalog_pdf: bytes) -> None:
        prefs = Document(data=catalog_pdf).viewer_preferences()
        assert isinstance(prefs, ViewerPreferences)
        assert prefs.hide_toolbar is True
        assert prefs.hide_menubar is False
        assert prefs.hide_window_ui is False
        assert prefs.fit_window is False
        assert prefs.center_window is False
        assert prefs.display_doc_title is False
        assert prefs.non_full_screen_page_mode == "use-none"
        assert prefs.direction == "right-to-left"
        assert prefs.view_area == "trim-box"
        assert prefs.view_clip == "crop-box"
        assert prefs.print_area == "crop-box"
        assert prefs.print_clip == "crop-box"
        assert prefs.print_scaling == "none"
        assert prefs.duplex == "duplex-flip-long-edge"
        assert prefs.pick_tray_by_pdf_size is False
        assert prefs.print_page_range == [(1, 2), (3, 3)]
        assert prefs.num_copies == 2
        assert repr(prefs) == (
            "ViewerPreferences(direction='right-to-left', print_scaling='none', "
            "duplex='duplex-flip-long-edge')"
        )

    def test_a_document_without_the_dictionary(self, hello_pdf: Path) -> None:
        assert Document(hello_pdf).viewer_preferences() is None


class TestExtensions:
    def test_declared_extension(self, catalog_pdf: bytes) -> None:
        (adbe,) = Document(data=catalog_pdf).extensions()
        assert (adbe.prefix, adbe.base_version, adbe.extension_level) == ("ADBE", "1.7", 3)
        assert repr(adbe) == (
            "DeveloperExtension(prefix='ADBE', base_version='1.7', extension_level=3)"
        )

    def test_a_document_without_any(self, hello_pdf: Path) -> None:
        assert Document(hello_pdf).extensions() == []


class TestAsync:
    def test_twins_read_the_written_document(self, written_pdf: bytes) -> None:
        async def run() -> dict[str, object]:
            doc = await AsyncDocument.from_bytes(written_pdf)
            (attachment,) = await doc.embedded_files()
            return {
                "outline": [item.title for item in await doc.outline()],
                "pages": [item.page for item in await doc.outline()],
                "labels": [await doc.page_label(i) for i in range(4)],
                "ranges": [r.style for r in await doc.page_labels()],
                "attachment": attachment.name,
                "data": await doc.embedded_file_data(attachment),
            }

        assert asyncio.run(run()) == {
            "outline": ["Chapter 1", "Chapter 2"],
            "pages": [0, 2],
            "labels": ["i", "ii", "A-5", None],
            "ranges": ["roman-lower", "decimal"],
            "attachment": "notes.txt",
            "data": b"hello attachment",
        }

    def test_twins_read_the_catalog(self, catalog_pdf: bytes) -> None:
        async def run() -> dict[str, object]:
            doc = await AsyncDocument.from_bytes(catalog_pdf)
            prefs = await doc.viewer_preferences()
            found = await doc.named_destinations()
            return {
                "direction": prefs.direction,
                "destinations": {name: d.page for name, d in found.items()},
                "extensions": [e.prefix for e in await doc.extensions()],
            }

        assert asyncio.run(run()) == {
            "direction": "right-to-left",
            "destinations": {"intro": 0, "summary": 1},
            "extensions": ["ADBE"],
        }

    def test_twins_on_a_plain_document(self, hello_pdf: Path) -> None:
        async def run() -> tuple[object, ...]:
            doc = await AsyncDocument.open(hello_pdf)
            return (
                await doc.outline(),
                await doc.named_destinations(),
                await doc.page_labels(),
                await doc.embedded_files(),
                await doc.viewer_preferences(),
                await doc.extensions(),
            )

        assert asyncio.run(run()) == ([], {}, None, [], None, [])
