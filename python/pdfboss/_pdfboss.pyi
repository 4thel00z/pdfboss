"""Type stubs for the compiled extension module ``pdfboss._pdfboss``.

The ``pdfboss`` package re-exports everything here; these stubs are the
typed surface editors and type checkers see.
"""

import os
from collections.abc import AsyncIterator, Iterator

__version__: str
"""The installed pdfboss version, e.g. ``"0.18.0"``."""

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

    kind: str
    """What the element is: ``"header"``, ``"object"``, ``"xref"``,
    ``"trailer"``, ``"startxref"``, ``"eof"``, ``"page"``, ``"font"``,
    ``"image"``, ``"annotation"`` or ``"content_op"``."""

    span: tuple[int, int] | None
    """Physical byte range in the file; for ``"content_op"`` the range
    within the page's decoded content stream."""

    ref: tuple[int, int] | None
    """The element's ``(num, gen)`` object reference, where applicable."""

    page: int | None
    """The 0-based page index for logical elements, ``None`` otherwise."""
    def value(self) -> object:
        """Lazy conversion to plain Python data:
        dict/list/str/bytes/int/float/bool/None. PDF names -> str,
        strings -> str where UTF-8-valid else bytes, streams ->
        {"dict": ..., "length": int}, references -> {"ref": (num, gen)}.
        """

class ElementIter:
    """Lazy sync iterator over elements; each ``__next__`` releases the
    GIL while the next element is located and parsed."""

    def __iter__(self) -> "ElementIter":
        """Returns the iterator itself."""

    def __next__(self) -> Element:
        """The next element; raises ``StopIteration`` when exhausted. A
        per-item failure raises ``PdfError`` for that item and iteration
        may be continued (salvage semantics)."""

class AsyncElementIter:
    """Async iterator over elements; each ``__anext__`` is a coroutine
    driving the underlying stream, so the event loop is never blocked."""

    def __aiter__(self) -> "AsyncElementIter":
        """Returns the iterator itself."""

    async def __anext__(self) -> Element:
        """The next element; raises ``StopAsyncIteration`` when exhausted.
        A per-item failure raises ``PdfError`` for that item and iteration
        may be continued (salvage semantics)."""

class Span:
    """One styled text span, yielded by ``Page.spans``/``Document.spans``
    and their async twins: a positioned run of text with everything the
    file states about how it is shown."""

    @property
    def text(self) -> str:
        """The decoded text."""

    @property
    def x(self) -> float:
        """Device-space x coordinate of the span origin."""

    @property
    def y(self) -> float:
        """Device-space y coordinate of the span baseline."""

    @property
    def end_x(self) -> float:
        """Device-space x after the last glyph's advance."""

    @property
    def size(self) -> float:
        """Effective font size."""

    @property
    def font(self) -> str:
        """Font resource name (e.g. ``"F1"``)."""

    @property
    def font_name(self) -> str:
        """The font's ``/BaseFont`` name verbatim — subset prefix included
        — falling back to the FontDescriptor's ``/FontName``; empty when
        the file names the font nowhere."""

    @property
    def page(self) -> int:
        """0-based index of the page the span came from."""

    @property
    def bbox(self) -> tuple[float, float, float, float]:
        """Device-space box ``(x0, y0, x1, y1)``, y-up: origin to advance
        horizontally, the font's descent..ascent vertically."""

    @property
    def bold(self) -> bool:
        """Bold, from FontDescriptor evidence with BaseFont-name fallback."""

    @property
    def italic(self) -> bool:
        """Italic, from FontDescriptor evidence with BaseFont-name
        fallback."""

    @property
    def monospace(self) -> bool:
        """FontDescriptor ``/Flags`` FixedPitch."""

    @property
    def serif(self) -> bool:
        """FontDescriptor ``/Flags`` Serif."""

    @property
    def underline(self) -> bool:
        """A drawn ruling sits just below the baseline covering most of
        the span. Read from the page's geometry — PDF has no underline
        attribute — so a table border hugging a cell's text can read as
        one."""

    @property
    def strikethrough(self) -> bool:
        """A drawn ruling crosses the span's x-height band —
        geometry-read, like ``underline``."""

    @property
    def rise(self) -> float:
        """The text rise (``Ts``) the span was shown under: positive above
        the baseline — a superscript/subscript signal."""

    @property
    def vertical(self) -> bool:
        """Writing mode 1: the text advances downward."""

    @property
    def invisible(self) -> bool:
        """Shown under render mode 3 or 7, which paint nothing — the shape
        of an OCR text layer under a scanned image."""

    @property
    def color(self) -> tuple[float, float, float] | None:
        """Fill color as RGB in ``[0, 1]``; ``None`` for pattern fills,
        which have no single color."""

class SpanIter:
    """Lazy sync iterator over a document's styled spans; buffers one
    page's spans at a time, extracting each page with the GIL released."""

    def __iter__(self) -> "SpanIter":
        """Returns the iterator itself."""

    def __next__(self) -> Span:
        """The next span; raises ``StopIteration`` when the walk is
        exhausted, ``PdfError`` when a page cannot be materialized."""

class AsyncSpanIter:
    """Async iterator over a document's styled spans; each ``__anext__``
    is a coroutine, so the event loop is never blocked."""

    def __aiter__(self) -> "AsyncSpanIter":
        """Returns the iterator itself."""

    async def __anext__(self) -> Span:
        """The next span; raises ``StopAsyncIteration`` when the walk is
        exhausted, ``PdfError`` when a page cannot be materialized."""

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
    ) -> None:
        """Opens a PDF from exactly one of ``path`` or ``data``; passing
        neither or both raises ``ValueError``. ``password`` opens an
        encrypted file, as either the user or the owner password."""

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

    def __len__(self) -> int:
        """Number of pages, so ``len(doc)`` mirrors ``doc.page_count``."""

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

    def spans(self, pages: list[int] | None = None) -> Iterator[Span]:
        """Lazily iterates the document's styled text spans, page by
        page: every page's, or the 0-based ``pages`` given, in the order
        given. Each step releases the GIL and shares one font cache
        across the walk."""

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

    def spans(self) -> list[Span]:
        """The page's styled text spans, in emission order. Releases the
        GIL like ``extract_text``, and is lenient the same way:
        unreadable content yields no spans rather than raising."""

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
    async def open(path: str | os.PathLike, *, password: str = "") -> "AsyncDocument":
        """Opens a PDF file for async access. The whole file is never read
        eagerly. ``password`` opens an encrypted file, as either the user
        or the owner password."""

    @staticmethod
    async def open_url(url: str, *, password: str = "") -> "AsyncDocument":
        """Opens a PDF over HTTP using range requests; the whole file is
        never downloaded. The server must honor ``Range`` (a server that
        ignores it raises ``PdfError`` with an ``"http:"`` message).
        ``password`` opens an encrypted file, as either the user or the
        owner password."""

    @staticmethod
    async def from_bytes(data: bytes, *, password: str = "") -> "AsyncDocument":
        """Loads a PDF from bytes already in memory. ``password`` opens an
        encrypted file, as either the user or the owner password."""

    @property
    def page_count(self) -> int:
        """Number of pages in the document. A property, exactly like the
        sync ``Document.page_count``: the open flow already parsed the
        page tree, so nothing here awaits."""

    @property
    def version(self) -> str:
        """PDF version from the file header, e.g. ``"1.7"``. A property,
        like the sync ``Document.version``."""

    def __len__(self) -> int:
        """Number of pages, so ``len(doc)`` mirrors ``doc.page_count``."""

    def __getitem__(self, index: int) -> "AsyncPage":
        """The page at ``index`` (0-based; negative indexes count from the
        end). Raises ``IndexError`` when out of range."""

    def page(self, index: int) -> "AsyncPage":
        """The page at 0-based ``index``. Synchronous — the page tree and
        its inherited attributes were resolved at open — and negative
        indexes are NOT accepted here (use subscription, ``doc[-1]``, for
        that), mirroring the sync ``Document`` split."""

    async def extract_text(self) -> str:
        """Extracts text from all pages, joined by form feed (``"\\f"``) —
        the async twin of ``Document.extract_text``. Per-page lenient: a
        page whose content will not read contributes an empty string, and
        an error means the document itself could not be read."""

    async def extract_markdown(self) -> str:
        """Whole-document markdown — the async twin of
        ``Document.extract_markdown``: headings, lists and tables inferred
        from layout, heading sizes judged across the document."""

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

    async def metadata(self) -> dict[str, str]:
        """Document metadata; only keys present in the file are included.
        Same keys as ``Document.metadata``. A coroutine, unlike the sync
        property: the info dictionary is fetched on demand."""

    async def get_object(self, num: int, gen: int = 0) -> object:
        """Fetches and parses the indirect object ``num gen``, returning
        its value converted to plain Python data (the same conversion as
        ``Element.value``)."""

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

    def spans(self, pages: list[int] | None = None) -> AsyncIterator[Span]:
        """Streams the document's styled text spans page by page — the
        async twin of ``Document.spans``, over range-fetching reads; use
        with ``async for``."""

class AsyncPage:
    """A single page of an async document.

    Attributes are synchronous — the page tree and its inherited
    attributes were resolved at open — while ``extract_text`` and the
    render methods are coroutines driving the same shared implementations
    the sync ``Page`` drives, over range-fetching reads.
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

    async def extract_text(self) -> str:
        """Extracts the page's text — the async twin of
        ``Page.extract_text``."""

    async def extract_markdown(self) -> str:
        """Page markdown, ranking heading sizes against that page alone —
        the async twin of ``Page.extract_markdown``.
        ``AsyncDocument.extract_markdown`` is the better answer whenever
        the whole document is at hand."""

    async def spans(self) -> list[Span]:
        """The page's styled text spans — the async twin of
        ``Page.spans``."""

    async def render(
        self,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
        compression: str = "default",
    ) -> bytes:
        """Renders the page at ``scale`` and resolves to PNG bytes — the
        async twin of ``Page.render``, with the same arguments and the
        same leniency."""

    async def render_reporting(
        self,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
        compression: str = "default",
    ) -> tuple[bytes, list[str]]:
        """Renders the page like ``render``, resolving to
        ``(png, warnings)`` — the async twin of ``Page.render_reporting``,
        with the same warning semantics."""
