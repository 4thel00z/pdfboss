"""The type stubs must cover every name and method the package exports."""

import inspect
from pathlib import Path

import pdfboss

STUB = Path(__file__).parent.parent / "python" / "pdfboss" / "_pdfboss.pyi"


def test_stub_declares_every_exported_class() -> None:
    stub = STUB.read_text()
    for name in pdfboss.__all__:
        if not inspect.isclass(getattr(pdfboss, name)):
            continue
        assert f"class {name}" in stub, f"missing stub for {name}"


def test_stub_declares_every_write_export() -> None:
    stub = STUB.read_text()
    for name in pdfboss.write.__all__:
        if not inspect.isclass(getattr(pdfboss.write, name)):
            continue
        assert f"class {name}" in stub, f"missing stub for pdfboss.write.{name}"


def test_stub_declares_the_element_and_async_surface() -> None:
    stub = STUB.read_text()
    assert "def elements(" in stub
    assert "def value(self) -> object" in stub
    assert 'async def open(path: str | os.PathLike, *, password: str = "")' in stub
    assert 'async def open_url(url: str, *, password: str = "")' in stub
    assert 'async def from_bytes(data: bytes, *, password: str = "")' in stub
    assert "async def metadata(self) -> dict[str, str]" in stub
    assert "async def get_object(self, num: int, gen: int = 0)" in stub
    assert "Iterator[Element]" in stub
    assert "AsyncIterator[Element]" in stub
