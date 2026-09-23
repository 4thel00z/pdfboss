"""The type stubs must cover every name and method the package exports."""

import inspect
from pathlib import Path

import pdfboss

STUB = Path(__file__).parent.parent / "python" / "pdfboss" / "_pdfboss.pyi"


def test_stub_declares_every_exported_class() -> None:
    """Every class the extension module exports has a stub. A class defined
    in Python (``ReadingOrder``) is its own declaration and needs none."""
    stub = STUB.read_text()
    for name in pdfboss.__all__:
        if not inspect.isclass(getattr(pdfboss, name)):
            continue
        if not hasattr(pdfboss._pdfboss, name):
            continue
        assert f"class {name}" in stub, f"missing stub for {name}"


def test_stub_declares_every_write_export() -> None:
    stub = STUB.read_text()
    for name in pdfboss.write.__all__:
        if not inspect.isclass(getattr(pdfboss.write, name)):
            continue
        assert f"\n    class {name}:" in stub, f"missing stub for pdfboss.write.{name}"


def test_stub_declares_the_element_and_async_surface() -> None:
    stub = STUB.read_text()
    assert "def elements(" in stub
    assert "def value(self) -> object" in stub
    assert 'async def open(path: str | os.PathLike, *, password: str = "")' in stub
    assert (
        'async def open_url(\n        url: str, *, password: str = "", headers: dict[str, str] | None = None\n    )'
        in stub
    )
    assert 'async def from_bytes(data: bytes, *, password: str = "")' in stub
    assert "async def metadata(self) -> dict[str, str]" in stub
    assert "async def get_object(self, num: int, gen: int = 0)" in stub
    assert "Iterator[Element]" in stub
    assert "AsyncIterator[Element]" in stub


def test_stub_declares_the_form_and_catalog_methods() -> None:
    stub = STUB.read_text()
    for line in (
        "def interactive_form(self) -> InteractiveForm | None",
        "def form_fields(self) -> list[FormField]",
        "def outline(self) -> list[OutlineItem]",
        "def named_destinations(self) -> dict[str, Destination]",
        "def page_labels(self) -> list[PageLabel] | None",
        "def page_label(self, index: int) -> str | None",
        "def embedded_files(self) -> list[EmbeddedFile]",
        "def embedded_file_data(self, file: EmbeddedFile) -> bytes",
        "def viewer_preferences(self) -> ViewerPreferences | None",
        "def extensions(self) -> list[DeveloperExtension]",
    ):
        assert f"    {line}" in stub, f"missing sync stub: {line}"
        assert f"    async {line}" in stub, f"missing async stub: {line}"


def test_stub_declares_the_document_and_page_structure_methods() -> None:
    stub = STUB.read_text()
    for line in (
        "def output_intents(self) -> list[OutputIntent]",
        'def optional_content_groups(self, event: str | None = "view") -> list[OptionalContentGroup]',
        "def piece_info(self) -> list[PagePiece]",
        "def articles(self) -> list[ArticleThread]",
        "def permission_handlers(self) -> PermissionHandlers | None",
        "def thumbnail(self) -> Thumbnail | None",
        'def thumbnail_image(self, compression: str = "default") -> PageImage | None',
        "def beads(self) -> list[tuple[int, int]]",
        "def presentation(self) -> Presentation | None",
    ):
        assert f"    {line}" in stub, f"missing sync stub: {line}"
        assert f"    async {line}" in stub, f"missing async stub: {line}"
    for line in (
        "def linearization(self) -> Linearization | None",
        "def is_linearized(self) -> bool",
    ):
        assert stub.count(f"    {line}") == 2, f"expected a plain stub on both documents: {line}"
        assert f"    async {line}" not in stub, f"the aio reader is plain, not a coroutine: {line}"
    for line in (
        "default_appearance: str | None",
        'quadding: Literal["left", "centered", "right"]',
        "default_style: str | None",
        "rich_text: str | None",
        'def parse(da: str) -> "DefaultAppearance"',
    ):
        assert line in stub, f"missing stub: {line}"


def test_stub_declares_the_catalog_structure_readers() -> None:
    stub = STUB.read_text()
    for line in (
        "def requirements(self) -> list[Requirement]",
        "def legal_attestation(self) -> LegalAttestation | None",
        "def viewports(self) -> list[Viewport]",
        "def separation_info(self) -> SeparationInfo | None",
    ):
        assert f"    {line}" in stub, f"missing sync stub: {line}"
        assert f"    async {line}" in stub, f"missing async stub: {line}"
    for line in (
        "handlers: list[RequirementHandler]",
        "def counts(self) -> dict[str, int]",
        "measure: Measure | None",
        "x: list[NumberFormat]",
        'fraction: Literal["decimal", "fraction", "round", "truncate"]',
        'label: Literal["suffix", "prefix"]',
        "pages: list[int | None]",
        "page_refs: list[tuple[int, int]]",
    ):
        assert line in stub, f"missing stub: {line}"


def test_stub_declares_the_annotation_readers() -> None:
    stub = STUB.read_text()
    for line in (
        "def annotations(self) -> list[Annotation]",
        "def additional_actions(self) -> list[TriggeredAction]",
        "def file_spec_data(self, spec: FileSpec) -> bytes",
    ):
        assert f"    {line}" in stub, f"missing sync stub: {line}"
        assert f"    async {line}" in stub, f"missing async stub: {line}"
    assert stub.count("    def additional_actions(self) -> list[TriggeredAction]") == 2
    assert stub.count("    async def additional_actions(self) -> list[TriggeredAction]") == 2
    for line in (
        "class Annotation:",
        "class AnnotationFlags:",
        "class Border:",
        "class Markup:",
        "class FileSpec:",
        "class Action:",
        "class Target:",
        "class WindowsLaunch:",
        "class TriggeredAction:",
        "flags: AnnotationFlags",
        "markup: Markup | None",
        "state_model: str | None",
        "destination: Destination | None",
        "additional_actions: list[TriggeredAction]",
        'reply_type: Literal["reply", "group"]',
        "named_destination: bytes | None",
        "entries: dict[str, object] | None",
        "next: list[Action]",
        'relationship: Literal["parent", "child"]',
        "url: str | None",
    ):
        assert line in stub, f"missing stub: {line}"
