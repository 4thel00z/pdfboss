import os

__version__: str

class PdfError(Exception):
    """Raised for any PDF processing error.

    Covers bad or truncated data, unsupported encryption, stream decode
    failures and I/O errors; the message carries the underlying detail.
    """

class Document:
    """A loaded PDF document.

    Construct from exactly one of ``path`` or ``data``; passing neither or
    both raises ``ValueError``.

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

    def extract_text(self) -> str:
        """Extracts text from all pages, joined by form feed (``"\\f"``)."""

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

    def extract_text(self) -> str:
        """Extracts the page's text."""

    def render(
        self,
        scale: float = 1.0,
        fonts: str = "all-embedded",
        font_dir: str | None = None,
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
        """
