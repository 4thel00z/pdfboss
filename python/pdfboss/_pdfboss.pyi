import os
from collections.abc import AsyncIterator, Iterator

__version__: str

class PdfError(Exception):
    """Raised for any PDF processing error.

    Covers bad or truncated data, unsupported encryption, stream decode
    failures and I/O errors; the message carries the underlying detail.
    Messages from the element and async APIs are prefixed by the layer
    they came from: ``"parse: …"``, ``"io: …"`` or ``"http: …"``.
    """

class Element:
    """One element of a PDF, yielded by ``Document.elements`` and
    ``AsyncDocument.elements``: physical file structure (with byte spans)
    or logical document structure.
    """

    kind: str                      # "header" | "object" | "xref" | "trailer" |
                                   # "startxref" | "eof" | "page" | "font" |
                                   # "image" | "annotation" | "content_op"
    span: tuple[int, int] | None   # physical byte range; for "content_op" the
                                   # range within the page's decoded content stream
    ref: tuple[int, int] | None    # (num, gen) where applicable
    page: int | None               # logical elements
    def value(self) -> object:
        """Lazy conversion to plain Python data:
        dict/list/str/bytes/int/float/bool/None. PDF names -> str,
        strings -> str where UTF-8-valid else bytes, streams ->
        {"dict": ..., "length": int}, references -> {"ref": (num, gen)}.
        """

class ElementIter:
    """Lazy sync iterator over elements; each ``__next__`` releases the
    GIL while the next element is located and parsed."""

    def __iter__(self) -> "ElementIter": ...
    def __next__(self) -> Element: ...

class AsyncElementIter:
    """Async iterator over elements; each ``__anext__`` is a coroutine
    driving the underlying stream, so the event loop is never blocked."""

    def __aiter__(self) -> "AsyncElementIter": ...
    async def __anext__(self) -> Element: ...

class Document:
    """A loaded PDF document.

    Construct from exactly one of ``path`` or ``data``; passing neither or
    both raises ``ValueError``. ``password`` opens an encrypted file, as
    either the user or the owner password (the empty user password opens
    transparently without one).

    Thread-safety: a ``Document`` (and any ``Page`` it hands out) may be
    used from any thread. Access to the underlying parsed document is
    serialized internally, and ``extract_text``/``render`` release the GIL
    while they run, so other Python threads keep making progress during
    long extractions or renders.
    """

    def __init__(
        self,
        path: str | os.PathLike[str] | None = None,
        *,
        data: bytes | None = None,
        password: str = "",
    ) -> None: ...
    @property
    def page_count(self) -> int:
        """Number of pages in the document."""

    @property
    def version(self) -> str:
        """PDF version from the file header, e.g. ``"1.7"``."""

    @property
    def metadata(self) -> dict[str, str]:
        """Document metadata; only keys present in the file are included.

        Possible keys: ``title``, ``author``, ``subject``, ``keywords``,
        ``creator``, ``producer``, ``creation_date``, ``mod_date``.
        """

    def __len__(self) -> int: ...
    def __getitem__(self, index: int) -> Page:
        """The page at ``index`` (0-based; negative indexes count from the
        end). Raises ``IndexError`` when out of range."""

    def render_pages(
        self,
        pages: list[int] | None = None,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
        compression: str = "default",
    ) -> list[bytes]:
        """Renders every page (or the 0-based ``pages`` given, in the order
        given) to PNG bytes, fanned out across the machine's cores."""

    def extract_text(self) -> str:
        """Extracts text from all pages, joined by form feed (``"\\f"``)."""

    def extract_markdown(self) -> str:
        """Whole-document markdown: headings, lists and tables inferred from
        layout; heading sizes judged across the document."""

    def elements(
        self,
        *,
        physical: bool = True,
        logical: bool = True,
        pages: list[int] | None = None,
        content_ops: bool = False,
    ) -> Iterator[Element]:
        """Lazily iterates the document's elements: physical file
        structure in file order, then logical document structure in
        document order. Nothing is parsed or decoded before it is
        yielded; each step releases the GIL while parsing.
        """

class Page:
    """A single page of a document.

    Pages may be used from any thread; access to the shared document is
    serialized internally, and ``extract_text``/``render`` release the GIL.
    """

    @property
    def number(self) -> int:
        """0-based page index."""

    @property
    def width(self) -> float:
        """Page width in points (after rotation)."""

    @property
    def height(self) -> float:
        """Page height in points (after rotation)."""

    @property
    def rotation(self) -> int:
        """Page rotation in degrees: 0, 90, 180 or 270."""

    @property
    def media_box(self) -> tuple[float, float, float, float]:
        """The media box ``(x0, y0, x1, y1)`` in unrotated PDF user space
        — ``width``/``height`` swap under ``rotation``, the boxes never do.
        US Letter ``(0, 0, 612, 792)`` when the file declares none."""

    @property
    def crop_box(self) -> tuple[float, float, float, float]:
        """The crop box, clipped to the media box; defaults to the media
        box."""

    @property
    def bleed_box(self) -> tuple[float, float, float, float]:
        """The bleed box, clipped to the media box; defaults to the crop
        box."""

    @property
    def trim_box(self) -> tuple[float, float, float, float]:
        """The trim box, clipped to the media box; defaults to the crop
        box."""

    @property
    def art_box(self) -> tuple[float, float, float, float]:
        """The art box, clipped to the media box; defaults to the crop
        box."""

    def extract_text(self) -> str:
        """Extracts the page's text."""

    def extract_markdown(self) -> str:
        """Page markdown, ranking heading sizes against that page alone.
        ``Document.extract_markdown`` is the better answer whenever the
        whole document is at hand."""

    def render(
        self,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
        compression: str = "default",
    ) -> bytes:
        """Renders the page at ``scale`` and returns PNG bytes.

        ``scale`` must be a positive, finite number (``ValueError``
        otherwise); 1.0 maps one PDF point to one pixel.

        ``fonts`` selects how aggressively non-embedded glyphs are painted:
        ``"embedded-only"``, ``"all-embedded"`` (default) or ``"full"``.
        ``"full"`` substitutes replacement faces for non-embedded fonts,
        read from ``font_dir`` if given, or else discovered from the
        optional ``pdfboss-fonts`` package; if neither is available this
        raises ``ValueError`` (install with ``pip install pdfboss[full]``,
        or pass ``font_dir=...``).

        ``compression`` trades PNG encode time against file size:
        ``"none"``, ``"fast"``, ``"default"`` or ``"best"``. Every level
        produces the same pixels.

        Content pdfboss cannot read is skipped rather than raising, so a
        page can come out blank; ``render_reporting`` says what was lost.
        """

    def render_reporting(
        self,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
        compression: str = "default",
    ) -> tuple[bytes, list[str]]:
        """Renders the page like ``render``, returning ``(png, warnings)``.

        ``warnings`` holds one line per distinct piece of content the
        render dropped or approximated, e.g. ``"1 image skipped:
        unsupported filter /Crypt"``, and is empty when the page
        rasterized exactly as it describes itself.
        """

class AsyncDocument:
    """A PDF document opened for async I/O.

    Constructors and data-fetching methods are coroutines driven by one
    global multi-thread tokio runtime; ``page_count``/``version`` and page
    access are sync (properties/plain calls, mirroring ``Document``)
    because the open flow already parsed the xref chain and page tree.
    The whole file is never read eagerly — file and HTTP backends fetch
    only the byte ranges they need.
    """

    @staticmethod
    async def open(path: str | os.PathLike, *, password: str = "") -> "AsyncDocument": ...
    @staticmethod
    async def open_url(url: str, *, password: str = "") -> "AsyncDocument": ...
    @staticmethod
    async def from_bytes(data: bytes, *, password: str = "") -> "AsyncDocument": ...
    @property
    def page_count(self) -> int: ...
    @property
    def version(self) -> str: ...
    def __len__(self) -> int: ...
    def __getitem__(self, index: int) -> "AsyncPage": ...
    def page(self, index: int) -> "AsyncPage": ...
    async def extract_text(self) -> str: ...
    async def extract_markdown(self) -> str: ...
    async def render_pages(
        self,
        pages: list[int] | None = None,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
        compression: str = "default",
    ) -> list[bytes]:
        """Renders every page (or the 0-based ``pages`` given, in the order
        given) to PNG bytes — the async twin of ``Document.render_pages``,
        fanned out across the cores as tokio tasks so the asyncio loop
        stays free. Works over any source, including ``open_url``."""

    async def metadata(self) -> dict[str, str]: ...
    async def get_object(self, num: int, gen: int = 0) -> object: ...
    def elements(
        self,
        *,
        physical: bool = True,
        logical: bool = True,
        pages: list[int] | None = None,
        content_ops: bool = False,
    ) -> AsyncIterator[Element]:
        """Streams the document's elements; use with ``async for``. Same
        ordering and salvage semantics as ``Document.elements``.
        """

class AsyncPage:
    """A single page of an async document.

    Attributes are synchronous — the page tree and its inherited
    attributes were resolved at open — while ``extract_text`` and the
    render methods are coroutines driving the same shared implementations
    the sync ``Page`` drives, over range-fetching reads.
    """

    @property
    def number(self) -> int: ...
    @property
    def width(self) -> float: ...
    @property
    def height(self) -> float: ...
    @property
    def rotation(self) -> int: ...
    @property
    def media_box(self) -> tuple[float, float, float, float]:
        """The media box ``(x0, y0, x1, y1)`` in unrotated PDF user space
        — ``width``/``height`` swap under ``rotation``, the boxes never do.
        US Letter ``(0, 0, 612, 792)`` when the file declares none."""

    @property
    def crop_box(self) -> tuple[float, float, float, float]:
        """The crop box, clipped to the media box; defaults to the media
        box."""

    @property
    def bleed_box(self) -> tuple[float, float, float, float]:
        """The bleed box, clipped to the media box; defaults to the crop
        box."""

    @property
    def trim_box(self) -> tuple[float, float, float, float]:
        """The trim box, clipped to the media box; defaults to the crop
        box."""

    @property
    def art_box(self) -> tuple[float, float, float, float]:
        """The art box, clipped to the media box; defaults to the crop
        box."""

    async def extract_text(self) -> str: ...
    async def extract_markdown(self) -> str: ...
    async def render(
        self,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
        compression: str = "default",
    ) -> bytes: ...
    async def render_reporting(
        self,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
        compression: str = "default",
    ) -> tuple[bytes, list[str]]: ...
