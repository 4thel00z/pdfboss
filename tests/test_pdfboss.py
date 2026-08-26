"""Integration tests for the pdfboss Python bindings.

Runs against the committed fixture PDFs in ``tests/fixtures/``. Requires the
extension module to be built and installed (e.g. via maturin).
"""

import gc
import sys
import threading
import time
import zlib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

import pdfboss
from pdfboss import Document, Page, PdfError

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def decode_png(png: bytes) -> tuple[int, int, bytes]:
    """Stdlib-only RGBA8 PNG decode: chunk walk, zlib, row unfilter.

    Deliberately not an imaging dependency, so the pixel-identity tests
    below always run. The ``none`` compression level writes unfiltered
    rows, so comparing it against the filtered levels also cross-checks
    this unfilter implementation.
    """
    if png[:8] != PNG_MAGIC:
        raise ValueError("not a PNG")
    pos, idat, width, height = 8, b"", 0, 0
    while pos < len(png):
        length = int.from_bytes(png[pos : pos + 4], "big")
        kind = png[pos + 4 : pos + 8]
        data = png[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            width = int.from_bytes(data[0:4], "big")
            height = int.from_bytes(data[4:8], "big")
            if (data[8], data[9], data[12]) != (8, 6, 0):
                raise ValueError("expected non-interlaced RGBA8")
        elif kind == b"IDAT":
            idat += data
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = width * 4
    out = bytearray()
    prev = bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        filter_type = raw[start]
        row = bytearray(raw[start + 1 : start + 1 + stride])
        for i in range(stride):
            a = row[i - 4] if i >= 4 else 0
            b = prev[i]
            c = prev[i - 4] if i >= 4 else 0
            if filter_type == 1:
                row[i] = (row[i] + a) & 0xFF
            elif filter_type == 2:
                row[i] = (row[i] + b) & 0xFF
            elif filter_type == 3:
                row[i] = (row[i] + (a + b) // 2) & 0xFF
            elif filter_type == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                nearest = a if pa <= pb and pa <= pc else b if pb <= pc else c
                row[i] = (row[i] + nearest) & 0xFF
        out += row
        prev = row
    return width, height, bytes(out)


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


class TestPageBoxes:
    def test_undeclared_boxes_fall_back_per_spec(self, hello_pdf: Path) -> None:
        page = Document(str(hello_pdf))[0]
        assert page.media_box == pytest.approx((0.0, 0.0, 612.0, 792.0))
        assert page.crop_box == page.media_box
        assert page.bleed_box == page.crop_box
        assert page.trim_box == page.crop_box
        assert page.art_box == page.crop_box

    def test_declared_boxes_are_reported(self, boxed_pdf: bytes) -> None:
        page = Document(data=boxed_pdf)[0]
        assert page.media_box == pytest.approx((0.0, 0.0, 600.0, 800.0))
        assert page.crop_box == pytest.approx((50.0, 50.0, 550.0, 750.0))
        assert page.bleed_box == pytest.approx((40.0, 40.0, 560.0, 760.0))
        assert page.trim_box == pytest.approx((60.0, 70.0, 540.0, 730.0))
        assert page.art_box == page.crop_box, "undeclared art box is the crop box"


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

    def test_render_fonts_embedded_only(self, hello_pdf: Path) -> None:
        png = Document(str(hello_pdf))[0].render(fonts="embedded-only")
        assert png.startswith(PNG_MAGIC)

    def test_render_unknown_fonts_raises_value_error(self, hello_pdf: Path) -> None:
        with pytest.raises(ValueError):
            Document(str(hello_pdf))[0].render(fonts="bogus")

    def test_full_without_fonts_package_raises(self, hello_pdf: Path, monkeypatch) -> None:
        # Force the discovery import to fail regardless of the test env.
        monkeypatch.setitem(sys.modules, "pdfboss_fonts", None)
        page = Document(str(hello_pdf))[0]
        with pytest.raises((ValueError, ImportError)) as exc:
            page.render(fonts="full")
        assert "pdfboss[full]" in str(exc.value)

    def test_full_with_explicit_font_dir_overrides_discovery(
        self, hello_pdf: Path, tmp_path: Path, monkeypatch
    ) -> None:
        # An explicit font_dir bypasses pdfboss_fonts entirely (even absent).
        monkeypatch.setitem(sys.modules, "pdfboss_fonts", None)
        page = Document(str(hello_pdf))[0]
        # An empty dir yields no faces -> Full degrades to all-embedded, but must
        # NOT raise (font_dir provided).
        png = page.render(fonts="full", font_dir=str(tmp_path))
        assert png[:8] == PNG_MAGIC

    def test_full_with_fonts_package_present(self, hello_pdf: Path) -> None:
        pytest.importorskip("pdfboss_fonts")
        page = Document(str(hello_pdf))[0]
        png = page.render(fonts="full")  # discovers pdfboss_fonts, no raise
        assert png[:8] == PNG_MAGIC

    def test_embedded_tiers_never_touch_discovery(
        self, hello_pdf: Path, monkeypatch
    ) -> None:
        monkeypatch.setitem(sys.modules, "pdfboss_fonts", None)
        page = Document(str(hello_pdf))[0]
        # all-embedded / embedded-only must not attempt discovery -> no raise.
        assert page.render(fonts="all-embedded")[:8] == PNG_MAGIC
        assert page.render(fonts="embedded-only")[:8] == PNG_MAGIC


class TestRenderCompression:
    def test_every_level_round_trips_the_same_pixels(self, hello_pdf: Path) -> None:
        page = Document(str(hello_pdf))[0]
        reference = decode_png(page.render())
        for level in ("none", "fast", "default", "best"):
            assert decode_png(page.render(compression=level)) == reference, level

    def test_no_compression_is_larger_than_best(self, hello_pdf: Path) -> None:
        page = Document(str(hello_pdf))[0]
        none = page.render(compression="none")
        best = page.render(compression="best")
        assert len(none) > len(best)

    def test_omitted_compression_is_byte_identical_to_the_default_level(
        self, hello_pdf: Path
    ) -> None:
        doc = Document(str(hello_pdf))
        page = doc[0]
        assert page.render() == page.render(compression="default")
        assert (
            page.render_reporting()[0] == page.render_reporting(compression="default")[0]
        )
        assert doc.render_pages() == doc.render_pages(compression="default")

    def test_render_pages_honors_the_level(self, three_pages_pdf: Path) -> None:
        doc = Document(str(three_pages_pdf))
        none = doc.render_pages(compression="none")
        best = doc.render_pages(compression="best")
        assert all(png.startswith(PNG_MAGIC) for png in none + best)
        assert all(len(n) > len(b) for n, b in zip(none, best))

    def test_unknown_compression_raises_value_error_naming_the_choices(
        self, hello_pdf: Path
    ) -> None:
        doc = Document(str(hello_pdf))
        page = doc[0]
        for call in (page.render, page.render_reporting, doc.render_pages):
            with pytest.raises(ValueError, match="'none', 'fast', 'default' or 'best'"):
                call(compression="bogus")


def pdf_with_undecodable_image() -> bytes:
    """A one-page PDF whose only content is an image carrying a filter
    pdfboss does not implement, so the page rasterizes blank."""
    bodies = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
        b"/Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>",
        b"<< /Length 30 >>\nstream\nq 100 0 0 100 0 0 cm /Im0 Do Q\nendstream",
        b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 "
        b"/BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /Crypt "
        b"/Length 8 >>\nstream\n01234567\nendstream",
    ]
    out = bytearray(b"%PDF-1.7\n")
    offsets = []
    for num, body in enumerate(bodies, start=1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % num + body + b"\nendobj\n"
    startxref = len(out)
    out += b"xref\n0 %d\n" % (len(bodies) + 1)
    out += b"0000000000 65535 f \n"
    for offset in offsets:
        out += b"%010d 00000 n \n" % offset
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\n" % (len(bodies) + 1)
    out += b"startxref\n%d\n%%%%EOF\n" % startxref
    return bytes(out)


class TestRenderReporting:
    def test_reports_the_dropped_image(self, tmp_path: Path) -> None:
        path = tmp_path / "dropped-image.pdf"
        path.write_bytes(pdf_with_undecodable_image())
        png, warnings = Document(str(path))[0].render_reporting()
        assert png.startswith(PNG_MAGIC)
        assert any("Crypt" in line for line in warnings), warnings

    def test_reports_nothing_for_a_clean_page(self, hello_pdf: Path) -> None:
        png, warnings = Document(str(hello_pdf))[0].render_reporting()
        assert png.startswith(PNG_MAGIC)
        assert warnings == []

    def test_render_still_returns_bytes_alone(self, tmp_path: Path) -> None:
        path = tmp_path / "dropped-image.pdf"
        path.write_bytes(pdf_with_undecodable_image())
        assert Document(str(path))[0].render().startswith(PNG_MAGIC)


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


class TestPageFanOut:
    """The parallel page fan-out must be invisible in the output: bulk
    results agree byte for byte with per-page calls, whatever the worker
    count or completion order."""

    def test_render_pages_matches_per_page_render(self, three_pages_pdf):
        doc = Document(str(three_pages_pdf))
        bulk = doc.render_pages(scale=1.5)
        assert bulk == [doc[i].render(scale=1.5) for i in range(doc.page_count)]

    def test_render_pages_honors_an_explicit_selection(self, three_pages_pdf):
        doc = Document(str(three_pages_pdf))
        # An explicit list renders exactly those pages, in the order given.
        subset = doc.render_pages(pages=[2, 0], scale=1.0)
        assert subset == [doc[2].render(scale=1.0), doc[0].render(scale=1.0)]

    def test_extract_text_joins_pages_in_order(self, three_pages_pdf):
        doc = Document(str(three_pages_pdf))
        parts = doc.extract_text().split("\f")
        assert parts == [doc[i].extract_text() for i in range(doc.page_count)]


class TestThreadedPageCalls:
    """Per-page calls hold no shared lock: whatever mix of threads calls
    ``render``/``extract_text``, the bytes must equal a sequential loop's."""

    def test_threaded_renders_match_sequential(self, three_pages_pdf):
        doc = Document(str(three_pages_pdf))
        expected = [doc[i].render(scale=1.5) for i in range(doc.page_count)]
        with ThreadPoolExecutor(max_workers=8) as pool:
            threaded = list(
                pool.map(lambda i: doc[i].render(scale=1.5), range(doc.page_count))
            )
        assert threaded == expected

    def test_one_page_object_shared_across_threads(self, three_pages_pdf):
        # A single Page instance is itself safe to hammer from many threads.
        page = Document(str(three_pages_pdf))[1]
        expected = page.render(scale=1.0)
        with ThreadPoolExecutor(max_workers=8) as pool:
            renders = list(pool.map(lambda i: page.render(scale=1.0), range(16)))
        assert renders == [expected] * 16

    def test_threaded_text_matches_sequential(self, three_pages_pdf):
        doc = Document(str(three_pages_pdf))
        expected = [doc[i].extract_text() for i in range(doc.page_count)]
        with ThreadPoolExecutor(max_workers=8) as pool:
            threaded = list(
                pool.map(lambda i: doc[i].extract_text(), range(doc.page_count))
            )
        assert threaded == expected

    def test_page_outlives_its_document(self, three_pages_pdf):
        # The page carries the document's shareable core, so dropping the
        # Document (and collecting it) must not invalidate the page.
        doc = Document(str(three_pages_pdf))
        page = doc[2]
        expected = page.render(scale=1.0)
        del doc
        gc.collect()
        assert page.render(scale=1.0) == expected
