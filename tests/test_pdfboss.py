"""Integration tests for the pdfboss Python bindings.

Runs against the committed fixture PDFs in ``tests/fixtures/``. Requires the
extension module to be built and installed (e.g. via maturin).
"""

import gc
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

import pdfboss
from pdfboss import Document, Page, PdfError

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


class TestOpen:
    def test_open_by_str_path(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        assert doc.page_count == 1

    def test_open_by_pathlike(self, hello_pdf: Path) -> None:
        doc = Document(hello_pdf)
        assert doc.page_count == 1

    def test_open_by_data(self, hello_pdf: Path) -> None:
        doc = Document(data=hello_pdf.read_bytes())
        assert doc.page_count == 1

    def test_path_and_data_agree(self, hello_pdf: Path) -> None:
        by_path = Document(str(hello_pdf))
        by_data = Document(data=hello_pdf.read_bytes())
        assert by_path.extract_text() == by_data.extract_text()


class TestConstructorErrors:
    def test_neither_arg_raises_value_error(self) -> None:
        with pytest.raises(ValueError):
            Document()

    def test_both_args_raise_value_error(self, hello_pdf: Path) -> None:
        with pytest.raises(ValueError):
            Document(str(hello_pdf), data=hello_pdf.read_bytes())

    def test_garbage_data_raises_pdf_error(self) -> None:
        with pytest.raises(PdfError):
            Document(data=b"garbage")

    def test_pdf_error_is_exception(self) -> None:
        assert issubclass(PdfError, Exception)


class TestDocument:
    def test_page_count_and_len(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        assert doc.page_count == 1
        assert len(doc) == 1

    def test_version_looks_like_pdf_version(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        major, _, minor = doc.version.partition(".")
        assert major.isdigit() and minor.isdigit()

    def test_metadata_is_dict_of_str(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        meta = doc.metadata
        assert isinstance(meta, dict)
        allowed = {
            "title",
            "author",
            "subject",
            "keywords",
            "creator",
            "producer",
            "creation_date",
            "mod_date",
        }
        for key, value in meta.items():
            assert key in allowed
            assert isinstance(value, str)

    def test_getitem_returns_page(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        page = doc[0]
        assert isinstance(page, Page)
        assert page.number == 0

    def test_negative_index(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        assert doc[-1].number == 0

    def test_index_past_end_raises(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        with pytest.raises(IndexError):
            doc[5]

    def test_negative_index_past_start_raises(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        with pytest.raises(IndexError):
            doc[-2]

    def test_index_too_large_for_isize_raises_index_error(
        self, hello_pdf: Path
    ) -> None:
        # An index that overflows the native integer width must still surface
        # as IndexError, not OverflowError.
        doc = Document(str(hello_pdf))
        for bad in (10**30, -(10**30)):
            with pytest.raises(IndexError):
                doc[bad]

    def test_version_dunder(self) -> None:
        assert isinstance(pdfboss.__version__, str)
        assert pdfboss.__version__


class TestText:
    def test_hello_text(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        assert "Hello" in doc[0].extract_text()

    def test_document_extract_text_matches_page(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        assert doc.extract_text() == doc[0].extract_text()

    def test_three_pages_len(self, three_pages_pdf: Path) -> None:
        doc = Document(str(three_pages_pdf))
        assert len(doc) == 3
        assert doc.page_count == 3

    def test_three_pages_form_feed_join(self, three_pages_pdf: Path) -> None:
        doc = Document(str(three_pages_pdf))
        text = doc.extract_text()
        assert text.count("\f") == 2
        assert "Page two" in text

    def test_three_pages_page_order(self, three_pages_pdf: Path) -> None:
        doc = Document(str(three_pages_pdf))
        parts = doc.extract_text().split("\f")
        assert "Page one" in parts[0]
        assert "Page two" in parts[1]
        assert "Page three" in parts[2]

    def test_xref_stream_same_text_as_hello(
        self, hello_pdf: Path, xref_stream_pdf: Path
    ) -> None:
        hello = Document(str(hello_pdf))
        xref = Document(str(xref_stream_pdf))
        assert xref.extract_text() == hello.extract_text()


class TestPageGeometry:
    def test_width_height_us_letter(self, hello_pdf: Path) -> None:
        page = Document(str(hello_pdf))[0]
        assert page.width == pytest.approx(612.0, abs=1.0)
        assert page.height == pytest.approx(792.0, abs=1.0)

    def test_rotation_is_normalized(self, hello_pdf: Path) -> None:
        page = Document(str(hello_pdf))[0]
        assert page.rotation in (0, 90, 180, 270)


class TestRender:
    def test_render_returns_png_bytes(self, hello_pdf: Path) -> None:
        png = Document(str(hello_pdf))[0].render()
        assert isinstance(png, bytes)
        assert png.startswith(PNG_MAGIC)
        assert len(png) > len(PNG_MAGIC)

    def test_render_shapes_scaled(self, shapes_pdf: Path) -> None:
        png = Document(str(shapes_pdf))[0].render(scale=2.0)
        assert isinstance(png, bytes)
        assert png.startswith(PNG_MAGIC)

    def test_render_bad_scale_raises_value_error(self, hello_pdf: Path) -> None:
        page = Document(str(hello_pdf))[0]
        with pytest.raises(ValueError):
            page.render(scale=0.0)
        with pytest.raises(ValueError):
            page.render(scale=-1.0)


class TestThreading:
    """Regressions for the pinned threading behavior: ``Document``/``Page``
    are usable from any thread (no ``PanicException``), dropping the last
    reference on a foreign thread is clean (no unraisable, no leak), and
    rendering releases the GIL.
    """

    def test_document_usable_from_worker_thread(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        expected = doc.extract_text()
        results: list[object] = []
        errors: list[BaseException] = []

        def worker() -> None:
            try:
                results.append(doc.page_count)
                results.append(doc.extract_text())
                results.append(doc[0].render())
            except BaseException as exc:  # noqa: BLE001 - PanicException regression
                errors.append(exc)

        thread = threading.Thread(target=worker)
        thread.start()
        thread.join()
        assert errors == []
        assert results[0] == 1
        assert results[1] == expected
        assert isinstance(results[2], bytes)
        assert results[2].startswith(PNG_MAGIC)

    def test_shared_document_concurrent_extraction(
        self, three_pages_pdf: Path
    ) -> None:
        doc = Document(str(three_pages_pdf))
        expected = [doc[i].extract_text() for i in range(3)]
        with ThreadPoolExecutor(max_workers=3) as pool:
            got = list(pool.map(lambda i: doc[i].extract_text(), range(3)))
        assert got == expected

    def test_drop_on_worker_thread_is_clean(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        holder: list[object] = [doc, doc[0]]
        del doc
        unraisables: list[object] = []

        def worker() -> None:
            holder.clear()
            gc.collect()

        old_hook = sys.unraisablehook
        sys.unraisablehook = unraisables.append
        try:
            thread = threading.Thread(target=worker)
            thread.start()
            thread.join()
        finally:
            sys.unraisablehook = old_hook
        assert unraisables == []

    def test_render_releases_gil(self, shapes_pdf: Path) -> None:
        page = Document(str(shapes_pdf))[0]
        page.render(scale=1.0)  # warm-up
        ticks = 0
        stop = threading.Event()

        def ticker() -> None:
            nonlocal ticks
            while not stop.is_set():
                ticks += 1
                time.sleep(0.001)

        thread = threading.Thread(target=ticker)
        thread.start()
        try:
            deadline = time.monotonic() + 0.3
            while time.monotonic() < deadline:
                page.render(scale=4.0)
        finally:
            stop.set()
            thread.join()
        # With the GIL held for the whole render the ticker only runs at
        # call boundaries (~1-2 ticks); with it released it runs freely.
        assert ticks >= 10
