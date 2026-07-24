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


class TestElementValues:
    def test_header_value_is_the_version_string(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        header = next(iter(doc.elements()))
        assert header.kind == "header"
        assert header.value() == doc.version

    def test_object_values_include_the_catalog_dict(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        values = [
            e.value() for e in doc.elements(logical=False) if e.kind == "object"
        ]
        catalogs = [
            v for v in values if isinstance(v, dict) and v.get("Type") == "Catalog"
        ]
        assert len(catalogs) == 1

    def test_refs_convert_to_ref_dicts(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        values = [
            e.value() for e in doc.elements(logical=False) if e.kind == "object"
        ]
        catalog = next(
            v for v in values if isinstance(v, dict) and v.get("Type") == "Catalog"
        )
        pages_ref = catalog["Pages"]
        assert set(pages_ref) == {"ref"}
        num, gen = pages_ref["ref"]
        assert isinstance(num, int)
        assert isinstance(gen, int)

    def test_stream_objects_convert_to_dict_and_length(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        values = [
            e.value() for e in doc.elements(logical=False) if e.kind == "object"
        ]
        streams = [
            v for v in values if isinstance(v, dict) and set(v) == {"dict", "length"}
        ]
        assert streams
        for stream in streams:
            assert isinstance(stream["dict"], dict)
            assert isinstance(stream["length"], int)
            assert stream["length"] >= 0

    def test_trailer_value_has_size_and_root(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        trailer = next(
            e for e in doc.elements(logical=False) if e.kind == "trailer"
        )
        value = trailer.value()
        assert isinstance(value["Size"], int)
        assert set(value["Root"]) == {"ref"}

    def test_startxref_eof_and_page_values(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        elements = list(doc.elements())
        startxref = next(e for e in elements if e.kind == "startxref")
        assert isinstance(startxref.value(), int)
        eof = next(e for e in elements if e.kind == "eof")
        assert eof.value() is None
        page = next(e for e in elements if e.kind == "page")
        assert page.value() is None

    def test_xref_value_reports_kind_and_entries(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        xref = next(e for e in doc.elements(logical=False) if e.kind == "xref")
        value = xref.value()
        assert value["kind"] in ("table", "stream")
        assert isinstance(value["entries"], int)
        assert value["entries"] > 0

    def test_font_value_has_subtype(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        font = next(e for e in doc.elements(physical=False) if e.kind == "font")
        value = font.value()
        assert isinstance(value["subtype"], str)
        assert value["subtype"]
        assert "base_font" in value

    def test_content_op_value_is_a_string(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        ops = [
            e
            for e in doc.elements(physical=False, content_ops=True)
            if e.kind == "content_op"
        ]
        assert ops
        assert all(isinstance(e.value(), str) and e.value() for e in ops)

    def test_value_is_repeatable(self, hello_pdf: Path) -> None:
        doc = Document(str(hello_pdf))
        for element in doc.elements(logical=False):
            assert element.value() == element.value()
