"""Tests for Document.elements(): lazy sync iteration over PDF elements.

Runs against the committed fixture PDFs in ``tests/fixtures/``. Requires the
extension module to be built and installed (e.g. via maturin).
"""

from pathlib import Path

from pdfboss import Document, Element

PHYSICAL_KINDS = {"header", "object", "xref", "trailer", "startxref", "eof"}
LOGICAL_KINDS = {"page", "font", "image", "annotation", "content_op"}


class TestSyncElements:
    def test_yields_element_instances(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        elements = list(doc.elements())
        assert elements
        assert all(isinstance(e, Element) for e in elements)

    def test_kinds_are_known(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        for element in doc.elements():
            assert element.kind in PHYSICAL_KINDS | LOGICAL_KINDS

    def test_elements_returns_a_lazy_iterator(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        it = doc.elements()
        assert iter(it) is it
        assert next(it).kind == "header"

    def test_each_call_returns_a_fresh_iterator(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        first = [e.kind for e in doc.elements()]
        second = [e.kind for e in doc.elements()]
        assert first == second

    def test_physical_layer_shape(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        kinds = [e.kind for e in doc.elements(logical=False)]
        assert kinds[0] == "header"
        assert "eof" in kinds
        assert set(kinds) <= PHYSICAL_KINDS

    def test_physical_spans_within_file(self, hello_pdf: Path) -> None:
        size = hello_pdf.stat().st_size
        doc = Document(str(hello_pdf))
        for element in doc.elements(logical=False):
            span = element.span
            assert span is not None
            start, end = span
            assert 0 <= start < end <= size

    def test_object_spans_start_at_the_object_header(self, hello_pdf: Path) -> None:
        raw = hello_pdf.read_bytes()
        doc = Document(str(hello_pdf))
        objects = [e for e in doc.elements(logical=False) if e.kind == "object"]
        assert objects
        for element in objects:
            num, gen = element.ref
            start, end = element.span
            assert raw[start:end].startswith(f"{num} {gen} obj".encode())
            assert b"endobj" in raw[start:end]

    def test_logical_layer_has_page_and_font(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        logical = list(doc.elements(physical=False))
        kinds = {e.kind for e in logical}
        assert "page" in kinds
        assert "font" in kinds
        pages = [e for e in logical if e.kind == "page"]
        assert [e.page for e in pages] == [0]
        assert all(e.span is None for e in pages)
        assert all(e.ref is not None for e in pages)

    def test_pages_filter(self, three_pages_pdf: Path) -> None:
        doc = Document(str(three_pages_pdf))
        pages = [
            e for e in doc.elements(physical=False, pages=[1]) if e.kind == "page"
        ]
        assert [e.page for e in pages] == [1]

    def test_content_ops_off_by_default_on_by_flag(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        default_kinds = {e.kind for e in doc.elements()}
        assert "content_op" not in default_kinds
        ops = [
            e
            for e in doc.elements(physical=False, content_ops=True)
            if e.kind == "content_op"
        ]
        assert ops
        assert all(e.page == 0 for e in ops)
        assert all(e.span is not None for e in ops)

    def test_keyword_only_arguments(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        try:
            doc.elements(False)
        except TypeError:
            pass
        else:
            raise AssertionError("elements() must reject positional arguments")

    def test_xref_stream_file_iterates(self, xref_stream_pdf: Path) -> None:
        doc = Document(str(xref_stream_pdf))
        kinds = [e.kind for e in doc.elements(logical=False)]
        assert kinds[0] == "header"
        assert "xref" in kinds
