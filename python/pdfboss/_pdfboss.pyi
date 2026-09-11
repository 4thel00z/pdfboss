"""Type stubs for the compiled extension module ``pdfboss._pdfboss``.

The ``pdfboss`` package re-exports everything here; these stubs are the
typed surface editors and type checkers see.
"""

import os
from collections.abc import AsyncIterator, Iterator, Sequence
from typing import Literal, Protocol, Self

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
        dict/list/str/bytes/int/float/bool/None. Objects and the trailer
        convert fully: PDF names -> str, strings -> str where UTF-8-valid
        else bytes, streams -> {"dict": ..., "length": int}, references ->
        {"ref": (num, gen)}. Other kinds: header -> the version string;
        xref -> {"kind": ..., "entries": ...}; startxref -> int; font ->
        {"subtype": ..., "base_font": ...}; image -> {"width": ...,
        "height": ...}; annotation -> {"subtype": ...}; content ops ->
        the operator rendered as a string; eof and page -> None.
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
        horizontally, the font's descent..ascent vertically, with the
        ``/FontBBox`` extent standing in for whichever of the two the
        descriptor leaves out or states as zero. A font-wide box, not the
        glyphs' own."""

    @property
    def ascent(self) -> float:
        """Height of the box above the baseline, in device units:
        ``bbox[3] - y``. For horizontal text the font's ``/Ascent`` (else
        the ``/FontBBox`` top) scaled by the size."""

    @property
    def descent(self) -> float:
        """Depth of the box below the baseline, in device units, zero or
        negative: ``bbox[1] - y``. For horizontal text the font's
        ``/Descent`` (else the ``/FontBBox`` bottom) scaled by the size."""

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
        """A drawn ruling sits just below the baseline, covers most of the
        span and stops within an em of the text it covers, or an
        ``/Underline`` or ``/Squiggly`` annotation covers the span. PDF has
        no underline attribute: the drawn case is read from the page's
        geometry, so a cell border that ends where the cell's text ends can
        read as one."""

    @property
    def strikethrough(self) -> bool:
        """A drawn ruling crosses the span's x-height band under the same
        coverage rules as ``underline``, or a ``/StrikeOut`` annotation
        covers the span."""

    @property
    def highlight(self) -> bool:
        """A filled rectangle about a line tall lies behind the span:
        painted before it, colored (white and gray bands are backgrounds),
        lighter than its text, covering most of it and stopping within an
        em of the text it covers. Paragraph shading and cell backgrounds run
        to their box edges and do not count. A ``/Highlight`` annotation
        covering the span sets it too."""

    @property
    def highlight_color(self) -> tuple[float, float, float] | None:
        """The highlight's color as RGB in ``[0, 1]``: the rectangle's fill
        color, read the way ``color`` is, or the annotation's ``/C``;
        ``None`` without a highlight, and for an annotation without a
        color."""

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

    @property
    def artifact(self) -> str | None:
        """The class of the ``/Artifact`` marked-content sequence the span
        was shown inside (ISO 32000-1 14.8.2.2): ``"Pagination"``,
        ``"Layout"``, ``"Page"``, ``"Background"``, or ``"Artifact"`` for one
        without a type; ``None`` for real content."""

    @property
    def artifact_subtype(self) -> str | None:
        """The ``/Subtype`` of a pagination artifact: ``"Header"``,
        ``"Footer"``, ``"Watermark"`` or a producer's own name; ``None``
        otherwise."""

    @property
    def alt(self) -> str | None:
        """The alternate description that applies to the span (ISO 32000-1
        14.9.3): the ``/Alt`` of the marked-content sequence it was shown
        inside, else, under structure-tree reading order, the ``/Alt`` of
        the nearest structure element above it; ``None`` when neither gives
        one. A description, not a replacement: ``text`` stays what was
        shown."""

    @property
    def lang(self) -> str | None:
        """The language of the span's text (ISO 32000-1 14.9.2): the
        ``/Lang`` of the marked-content sequence it was shown inside, else,
        under structure-tree reading order, the ``/Lang`` of the nearest
        structure element above it; ``None`` leaves the document's own
        ``Document.language``."""

    @property
    def expansion(self) -> str | None:
        """The expansion of the abbreviation or acronym the span shows (ISO
        32000-1 14.9.5): the ``/E`` of the marked-content sequence it was
        shown inside, else, under structure-tree reading order, the ``/E``
        of the nearest structure element above it; ``None`` when neither
        gives one. Like ``alt``, a description: ``text`` stays what was
        shown."""

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

    @property
    def language(self) -> str | None:
        """The language the catalog declares for the document's text (ISO
        32000-1 14.9.2), e.g. ``"en-US"``; ``None`` when it declares none.
        A span's own ``lang`` overrides it for that span."""

    def __len__(self) -> int:
        """Number of pages, so ``len(doc)`` mirrors ``doc.page_count``."""

    def __getitem__(self, index: int) -> Page:
        """The page at ``index`` (0-based; negative indexes count from the
        end). Raises ``IndexError`` when out of range."""

    def render_pages(
        self,
        pages: list[int] | None = None,
        scale: float = 1.0,
        fonts: str | None = None,
        font_dir: str | None = None,
        compression: str = "default",
        format: str = "png",
        quality: int = 90,
    ) -> list[bytes]:
        """Renders every page (or the 0-based ``pages`` given, in the order
        given) to image bytes (PNG unless ``format`` says otherwise), fanned
        out across the machine's cores."""

    def extract_text(self, *, reading_order: str = "content") -> str:
        """Extracts text from all pages, joined by form feed (``"\\f"``)."""

    def extract_markdown(self, *, reading_order: str = "content") -> str:
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

    def spans(
        self, pages: list[int] | None = None, *, reading_order: str = "content"
    ) -> Iterator[Span]:
        """Lazily iterates the document's styled text spans, page by
        page: every page's, or the 0-based ``pages`` given, in the order
        given. Each step releases the GIL and shares one font cache
        across the walk."""

    def interactive_form(self) -> InteractiveForm | None:
        """The interactive form dictionary (ISO 32000-1 12.7.2): the root
        fields and the defaults their widgets are drawn with; ``None`` for
        a document without a form."""

    def form_fields(self) -> list[FormField]:
        """Every field of the interactive form (ISO 32000-1 12.7.3), depth
        first from the root fields in the order written, each with the
        inheritable entries taken from the nearest ancestor that has them.
        Empty without a form. Releases the GIL while the tree is read."""

    def outline(self) -> list[OutlineItem]:
        """The outline, the bookmark panel (ISO 32000-1 12.3.3): the
        top-level items with their children in panel order, each
        destination's page resolved to its 0-based index. Empty without
        one. Releases the GIL."""

    def named_destinations(self) -> dict[str, Destination]:
        """The named destinations (ISO 32000-1 12.3.2.3) from the
        catalog's ``/Dests`` dictionary and its ``/Names`` tree, keyed by
        the decoded name; a name in both takes the tree's. Releases the
        GIL while they are read."""

    def page_labels(self) -> list[PageLabel] | None:
        """The page-numbering ranges (ISO 32000-1 12.4.2), sorted by first
        page; ``None`` when the document defines none."""

    def page_label(self, index: int) -> str | None:
        """The label the page at 0-based ``index`` shows, such as ``"iv"``
        or ``"A-3"``; ``None`` when the document defines no page labels
        or has no such page."""

    def embedded_files(self) -> list[EmbeddedFile]:
        """The files embedded at document level (ISO 32000-1 7.11.4), the
        attachments, in the order of the catalog's name tree;
        ``embedded_file_data`` reads one."""

    def embedded_file_data(self, file: EmbeddedFile) -> bytes:
        """The decoded bytes of an embedded file. Releases the GIL. Raises
        ``PdfError`` when the specification embeds no stream or the stream
        will not decode."""

    def viewer_preferences(self) -> ViewerPreferences | None:
        """The viewer preferences the catalog declares (ISO 32000-1 12.2);
        ``None`` without the dictionary."""

    def extensions(self) -> list[DeveloperExtension]:
        """The developer extensions the catalog declares (ISO 32000-1
        7.12), sorted by prefix."""

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

    def extract_text(self, *, reading_order: str = "content") -> str:
        """Extracts the page's text."""

    def extract_markdown(self, *, reading_order: str = "content") -> str:
        """Page markdown, ranking heading sizes against that page alone.
        ``Document.extract_markdown`` is the better answer whenever the
        whole document is at hand."""

    def spans(self, *, reading_order: str = "content") -> list[Span]:
        """The page's styled text spans, in emission order. Releases the
        GIL like ``extract_text``, and is lenient the same way:
        unreadable content yields no spans rather than raising."""

    def render(
        self,
        scale: float = 1.0,
        fonts: str | None = None,
        font_dir: str | None = None,
        compression: str = "default",
        format: str = "png",
        quality: int = 90,
    ) -> bytes:
        """Renders the page at ``scale`` and returns the encoded image: PNG
        unless ``format`` says otherwise.

        ``scale`` must be a positive, finite number (``ValueError``
        otherwise); 1.0 maps one PDF point to one pixel.

        ``fonts`` selects how aggressively non-embedded glyphs are painted:
        ``"embedded-only"``, ``"all-embedded"`` or ``"full"``. ``"full"``
        substitutes replacement faces for non-embedded fonts, read from
        ``font_dir`` if given, or else discovered from the optional
        ``pdfboss-fonts`` package; if neither is available an explicit
        ``fonts="full"`` raises ``ValueError`` (install with ``pip install
        pdfboss[full]``, or pass ``font_dir=...``). The default, ``None``,
        resolves to ``"full"`` when a face source is at hand and to
        ``"all-embedded"`` when none is.

        ``format`` picks the file format: ``"png"``, ``"ppm"`` (binary P6,
        RGB), ``"bmp"`` (24-bit) or ``"jpeg"`` (``"jpg"`` is accepted too).
        PPM and BMP are a header plus the pixels, dropping alpha;
        ``compression`` trades PNG encode time against file size
        (``"none"``, ``"fast"``, ``"default"`` or ``"best"``) and only
        shapes PNG; ``quality`` (1 to 100) is the JPEG quality and only
        shapes JPEG. Apart from JPEG, every choice produces the same pixels.

        Content pdfboss cannot read is skipped rather than raising, so a
        page can come out blank; ``render_reporting`` says what was lost.
        """

    def render_reporting(
        self,
        scale: float = 1.0,
        fonts: str | None = None,
        font_dir: str | None = None,
        compression: str = "default",
        format: str = "png",
        quality: int = 90,
    ) -> tuple[bytes, list[str]]:
        """Renders the page like ``render``, returning ``(image, warnings)``.

        ``warnings`` holds one line per distinct piece of content the
        render dropped or approximated, e.g. ``"1 image skipped:
        the data could not be interpreted"``, and is empty when the page
        rasterized exactly as it describes itself.
        """

    def extract_images(self, compression: str = "default") -> list[PageImage]:
        """Every image the page draws, each decoded at its native pixel
        dimensions and re-encoded as PNG, in drawing order.

        An image drawn twice appears twice; an XObject the content never
        draws does not appear; stencil masks (``/ImageMask true``) paint a
        fill color rather than carrying pixels of their own and are
        skipped. An image's ``/SMask`` becomes the PNG alpha channel.

        ``compression`` trades PNG encode time against file size exactly
        as in ``render``. Lenient like rendering: content that cannot be
        read or decoded contributes nothing rather than raising.
        """

class PageImage:
    """One embedded image extracted from a page: PNG-encoded pixels at
    the image's own native dimensions, straight alpha, ``/SMask``
    applied."""

    @property
    def width(self) -> int:
        """Native pixel width of the embedded image."""

    @property
    def height(self) -> int:
        """Native pixel height of the embedded image."""

    @property
    def data(self) -> bytes:
        """The image re-encoded as PNG (RGBA8)."""

class InteractiveForm:
    """The interactive form dictionary (ISO 32000-1 12.7.2, Table 218):
    the root fields and the defaults their widgets are drawn with,
    returned by ``Document.interactive_form``.
    """

    fields: list[tuple[int, int]]
    """The root fields, those with no parent, as ``(num, gen)`` references."""

    need_appearances: bool
    """Whether a reader should build the widgets' appearance streams itself."""

    signatures_exist: bool
    """Whether the document carries at least one signature field with a
    signature."""

    append_only: bool
    """Whether the document may only be saved as incremental updates, so
    its signatures stay valid."""

    calculation_order: list[tuple[int, int]]
    """The fields with calculation actions, in the order their values are
    recalculated, as ``(num, gen)`` references."""

    default_resources: dict[str, object] | None
    """The default resources for the fields' appearance streams, as a plain
    dict through the ``Element.value()`` conversion; ``None`` when the form
    names none."""

    default_appearance: str | None
    """The document-wide default appearance string for variable text, a
    content fragment such as ``"/Helv 0 Tf 0 g"``, as written."""

    quadding: Literal["left", "centered", "right"] | None
    """The document-wide default justification of variable text; ``None``
    when the form sets none."""

    xfa: bool
    """Whether the form carries an XFA resource."""

class FieldFlags:
    """A field's flag word (ISO 32000-1 Tables 221, 226, 228 and 230) with
    one boolean per flag the standard defines; which flags mean anything
    depends on the field's type. ``int(flags)`` is the raw word.
    """

    bits: int
    """The raw flag word."""

    names: list[str]
    """The names of the flags that are set, in bit order, each also
    available as a boolean attribute of the same name."""

    read_only: bool
    required: bool
    no_export: bool
    multiline: bool
    password: bool
    file_select: bool
    do_not_spell_check: bool
    do_not_scroll: bool
    comb: bool
    rich_text: bool
    combo: bool
    edit: bool
    sort: bool
    multi_select: bool
    commit_on_sel_change: bool
    no_toggle_to_off: bool
    radio: bool
    pushbutton: bool
    radios_in_unison: bool

    def __int__(self) -> int:
        """The raw flag word."""

class AppearanceCharacteristics:
    """The captions and icons a button widget is drawn with (ISO 32000-1
    12.5.6.19, Table 189)."""

    caption: str | None
    """The caption shown while the button is at rest."""

    rollover_caption: str | None
    """The caption shown while the cursor is over the button."""

    alternate_caption: str | None
    """The caption shown while the mouse button is down."""

    icon: tuple[int, int] | None
    """The normal icon, a form XObject as a ``(num, gen)`` reference."""

    rollover_icon: tuple[int, int] | None
    """The rollover icon as a ``(num, gen)`` reference."""

    alternate_icon: tuple[int, int] | None
    """The alternate icon as a ``(num, gen)`` reference."""

    caption_position: Literal[
        "caption-only", "icon-only", "below", "above", "right", "left", "overlaid"
    ]
    """Where the caption sits relative to the icon; ``"caption-only"`` by
    default."""

class Widget:
    """A widget annotation that draws a field (ISO 32000-1 12.5.6.19), as
    far as the field's state needs it."""

    ref: tuple[int, int]
    """The annotation dictionary's ``(num, gen)`` reference; the field's
    own for a field merged with its single widget."""

    appearance_state: str | None
    """The appearance state the widget shows."""

    on_state: str | None
    """The widget's on state: the one key of its normal appearance
    dictionary other than ``Off``. ``None`` when the normal appearance is a
    single stream or names no or several other states."""

    characteristics: AppearanceCharacteristics | None
    """The captions and icons a button widget is drawn with; ``None``
    without the dictionary."""

class ChoiceOption:
    """One option of a choice field (ISO 32000-1 12.7.4.4), or one export
    value of a check box or radio button."""

    export_value: str
    """The value exported for the option."""

    name: str
    """The text shown to the user; also what the field's value names when
    the option is selected. A lone string in the file is both."""

class Signature:
    """A signature dictionary (ISO 32000-1 12.8.1, Table 252) as the value
    of a signature field, read as data: nothing here is verified."""

    filter: str | None
    """The name of the preferred signature handler."""

    sub_filter: str | None
    """The encoding of the signature value, such as ``adbe.pkcs7.detached``."""

    byte_range: list[tuple[int, int]]
    """The ``(offset, length)`` pairs of the file bytes the signature covers."""

    contents: bytes
    """The signature value as stored, usually DER-encoded PKCS#7."""

    name: str | None
    """The name of the person or authority signing."""

    signing_time: str | None
    """The time of signing as the PDF date string written."""

    location: str | None
    """The CPU host name or physical location of the signing."""

    reason: str | None
    """The reason for the signing."""

    contact_info: str | None
    """How to contact the signer to verify the signature."""

class FormField:
    """One field of the interactive form (ISO 32000-1 12.7.3, Table 220)
    with the inheritable entries taken from the nearest ancestor that has
    them, yielded by ``Document.form_fields``. The typed readers ``text``,
    ``button_kind``, ``state``, ``checked``, ``on_widgets``, ``signature``
    and ``selected`` interpret ``value`` by field type.
    """

    ref: tuple[int, int]
    """The field dictionary's own ``(num, gen)`` reference."""

    parent: tuple[int, int] | None
    """The parent field's ``(num, gen)`` reference; ``None`` for a root field."""

    kids: list[tuple[int, int]]
    """The child fields as ``(num, gen)`` references, in the order written."""

    widgets: list[Widget]
    """The widget annotations that draw this field: the kids that are
    widgets without a name of their own, or the field itself when its
    single widget is merged into it."""

    field_type: Literal["button", "text", "choice", "signature"] | None
    """The field's type, inherited; ``None`` for a field that names no
    type, such as a container of other fields."""

    partial_name: str | None
    """The field's own partial name."""

    name: str
    """The fully qualified name: the partial names from the root field
    down, joined by periods. A field without a partial name shares its
    parent's name; an unnamed root has the empty name."""

    alternate_name: str | None
    """The name shown to the user in place of the field name."""

    mapping_name: str | None
    """The name used when the field's data is exported."""

    flags: FieldFlags
    """The field's flags, inherited."""

    value: object
    """The field's value, inherited, as plain Python data through the
    ``Element.value()`` conversion, in the format of the field's type;
    ``None`` without one."""

    default_value: object
    """The value a reset-form action restores, as plain Python data."""

    max_len: int | None
    """The most characters a text field's text may hold."""

    options: list[ChoiceOption]
    """The options of a choice field, or the export values of a check box
    or radio button, one per widget."""

    top_index: int
    """The index into ``options`` of the first option a scrollable list box
    shows; 0 by default."""

    selected_indices: list[int]
    """The indices into ``options`` of a multi-select choice field's
    selected options, as written."""

    additional_actions: dict[str, object] | None
    """The field's additional-actions dictionary as a plain dict, as
    written; ``None`` without one."""

    lock: tuple[int, int] | None
    """The signature field lock dictionary as a ``(num, gen)`` reference."""

    seed_value: tuple[int, int] | None
    """The seed value dictionary as a ``(num, gen)`` reference."""

    text: str | None
    """The text of a text field; ``None`` for another type or no value."""

    button_kind: Literal["push-button", "check-box", "radio-buttons"] | None
    """Which kind of button a button field is; ``None`` for another type."""

    state: str | None
    """The appearance state a check box or radio button field is in, the
    name its widgets key their on and off appearances by; ``"Off"``
    without a value. ``None`` for a push button or another type."""

    checked: bool | None
    """Whether a check box is checked; ``None`` for a field that is no
    check box."""

    on_widgets: list[int]
    """The indices into ``widgets`` of the widgets in the on state; the
    same indices pick the export values out of ``options``. Empty for a
    field in the off state, a push button, or another type."""

    signature: Signature | None
    """The signature a signature field holds; ``None`` for another type or
    an unsigned field."""

    selected: list[str]
    """The names of a choice field's selected options; empty for another
    type or no value."""

class Destination:
    """One explicit destination (ISO 32000-1 12.3.2.2): a page and how the
    viewer shows it, as an outline item's target or a named destination."""

    page: int | None
    """The 0-based index of the page shown: the page the destination's
    reference names, or the page number a remote destination gives.
    ``None`` when the reference names no page of this document."""

    page_ref: tuple[int, int] | None
    """The ``(num, gen)`` reference of the page shown; ``None`` when the
    destination gives a page number instead."""

    fit: Literal["xyz", "fit", "fit-h", "fit-v", "fit-r", "fit-b", "fit-bh", "fit-bv"]
    """How the page is shown. ``"xyz"`` takes ``left``, ``top`` and
    ``zoom``; ``"fit-h"`` and ``"fit-bh"`` take ``top``; ``"fit-v"`` and
    ``"fit-bv"`` take ``left``; ``"fit-r"`` takes all four edges. A
    coordinate the kind does not take, or that the viewer keeps, is
    ``None``."""

    left: float | None
    top: float | None
    right: float | None
    bottom: float | None
    zoom: float | None

class OutlineItem:
    """One entry of the outline, the bookmark panel (ISO 32000-1 12.3.3,
    Table 153), with its children; returned by ``Document.outline``."""

    title: str
    """The title shown in the panel."""

    destination: Destination | None
    """Where activating the item goes; ``None`` for an item whose action
    is not a go-to within the document."""

    page: int | None
    """The 0-based index of the destination's page, a shortcut for
    ``destination.page``; ``None`` without one."""

    open: bool
    """Whether the item shows its children."""

    color: tuple[float, float, float]
    """The title's ``(r, g, b)`` colour in 0 to 1; black by default."""

    italic: bool
    bold: bool

    structure_element: tuple[int, int] | None
    """The structure element the item refers to as a ``(num, gen)``
    reference."""

    children: list[OutlineItem]
    """The item's children, in panel order."""

class PageLabel:
    """One page-numbering range (ISO 32000-1 12.4.2): how the pages from
    ``first_page`` on are labelled until the next range starts; returned
    by ``Document.page_labels``. The write-side ``write.PageLabel`` takes
    the same fields."""

    first_page: int
    """The 0-based index of the first page the range labels."""

    style: Literal["decimal", "roman-upper", "roman-lower", "letters-upper", "letters-lower"] | None
    """The numbering style; ``None`` for labels that are the prefix alone."""

    prefix: str | None
    """The text put before each label's number."""

    start_at: int
    """The number the range's first page gets."""

    def label(self, index: int) -> str:
        """The label of the page at 0-based ``index``, which must be in the
        range."""

class EmbeddedFile:
    """One file embedded at document level (ISO 32000-1 7.11.4), an
    attachment; returned by ``Document.embedded_files`` and read by
    ``Document.embedded_file_data``."""

    name: str
    """The name the document files the attachment under."""

    file_name: str
    """The file name the specification gives, which may differ from
    ``name``; empty when it gives none."""

    description: str | None
    """The description shown next to the file."""

    file_system: str | None
    """The file system that interprets the specification, ``URL`` being
    the one the standard defines."""

    volatile: bool
    """Whether the file changes often enough that it must not be cached."""

    ref: tuple[int, int] | None
    """The embedded file stream's ``(num, gen)`` reference; ``None`` when
    the specification embeds no stream."""

    mime: str | None
    """The file's MIME type."""

    size: int | None
    """The uncompressed size in bytes, as declared."""

    created: str | None
    """The creation date as an ISO 8601 string, ``YYYY-MM-DDTHH:MM:SS``
    followed by ``Z`` or a ``+HH:MM`` offset."""

    modified: str | None
    """The modification date as an ISO 8601 string."""

    checksum: bytes | None
    """The MD5 digest of the uncompressed bytes, as stored."""

class ViewerPreferences:
    """The viewer preferences the catalog declares (ISO 32000-1 12.2,
    Table 150); every entry a missing one defaults holds its default.
    Returned by ``Document.viewer_preferences``."""

    hide_toolbar: bool
    hide_menubar: bool
    hide_window_ui: bool
    fit_window: bool
    center_window: bool

    display_doc_title: bool
    """Whether the window title shows the document's title rather than its
    file name."""

    non_full_screen_page_mode: Literal["use-none", "use-outlines", "use-thumbs", "use-oc"]
    """The panel shown on leaving full-screen mode."""

    direction: Literal["left-to-right", "right-to-left"]
    """The reading order that places pages shown side by side."""

    view_area: Literal["media-box", "crop-box", "bleed-box", "trim-box", "art-box"]
    """The page box shown on screen; ``"crop-box"`` by default."""

    view_clip: Literal["media-box", "crop-box", "bleed-box", "trim-box", "art-box"]
    """The page box the screen view is clipped to."""

    print_area: Literal["media-box", "crop-box", "bleed-box", "trim-box", "art-box"]
    """The page box printed."""

    print_clip: Literal["media-box", "crop-box", "bleed-box", "trim-box", "art-box"]
    """The page box printing is clipped to."""

    print_scaling: Literal["none", "app-default"]
    """The page scaling the print dialog starts with."""

    duplex: Literal["simplex", "duplex-flip-short-edge", "duplex-flip-long-edge"] | None
    """The paper handling the print dialog starts with; ``None`` when left
    to the viewer."""

    pick_tray_by_pdf_size: bool | None
    """Whether the printer picks the paper tray by the page size."""

    print_page_range: list[tuple[int, int]]
    """The ``(first, last)`` 1-based page ranges the print dialog starts
    with."""

    num_copies: int | None
    """The number of copies the print dialog starts with."""

class DeveloperExtension:
    """One developer extension the catalog declares (ISO 32000-1 7.12);
    returned by ``Document.extensions``."""

    prefix: str
    """The developer's registered prefix, such as ``"ADBE"``."""

    base_version: str
    """The PDF version the extension builds on, such as ``"1.7"``."""

    extension_level: int
    """The developer's extension level."""

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
        """The page at 0-based ``index``, a method form of subscription
        (``doc[i]``). Synchronous, since the page tree and its inherited
        attributes were resolved at open. Negative indexes are NOT
        accepted here; use subscription, ``doc[-1]``, for those."""

    async def extract_text(self, *, reading_order: str = "content") -> str:
        """Extracts text from all pages, joined by form feed (``"\\f"``) —
        the async twin of ``Document.extract_text``. Per-page lenient: a
        page whose content will not read contributes an empty string, and
        an error means the document itself could not be read."""

    async def extract_markdown(self, *, reading_order: str = "content") -> str:
        """Whole-document markdown — the async twin of
        ``Document.extract_markdown``: headings, lists and tables inferred
        from layout, heading sizes judged across the document."""

    async def render_pages(
        self,
        pages: list[int] | None = None,
        scale: float = 1.0,
        fonts: str | None = None,
        font_dir: str | None = None,
        compression: str = "default",
        format: str = "png",
        quality: int = 90,
    ) -> list[bytes]:
        """Renders every page (or the 0-based ``pages`` given, in the order
        given) to image bytes — the async twin of ``Document.render_pages``,
        fanned out across the cores as tokio tasks so the asyncio loop
        stays free. Works over any source, including ``open_url``."""

    async def metadata(self) -> dict[str, str]:
        """Document metadata; only keys present in the file are included.
        Same keys as ``Document.metadata``. A coroutine, unlike the sync
        property: the info dictionary is fetched on demand."""

    async def language(self) -> str | None:
        """The language the catalog declares for the document's text (ISO
        32000-1 14.9.2); the async twin of ``Document.language``."""

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

    def spans(
        self, pages: list[int] | None = None, *, reading_order: str = "content"
    ) -> AsyncIterator[Span]:
        """Streams the document's styled text spans page by page — the
        async twin of ``Document.spans``, over range-fetching reads; use
        with ``async for``."""

    async def interactive_form(self) -> InteractiveForm | None:
        """The interactive form dictionary, the async twin of
        ``Document.interactive_form``."""

    async def form_fields(self) -> list[FormField]:
        """Every field of the interactive form, the async twin of
        ``Document.form_fields``."""

    async def outline(self) -> list[OutlineItem]:
        """The outline, the async twin of ``Document.outline``."""

    async def named_destinations(self) -> dict[str, Destination]:
        """The named destinations, the async twin of
        ``Document.named_destinations``."""

    async def page_labels(self) -> list[PageLabel] | None:
        """The page-numbering ranges, the async twin of
        ``Document.page_labels``."""

    async def page_label(self, index: int) -> str | None:
        """The label of the page at 0-based ``index``, the async twin of
        ``Document.page_label``."""

    async def embedded_files(self) -> list[EmbeddedFile]:
        """The embedded files, the async twin of
        ``Document.embedded_files``."""

    async def embedded_file_data(self, file: EmbeddedFile) -> bytes:
        """The decoded bytes of an embedded file, the async twin of
        ``Document.embedded_file_data``."""

    async def viewer_preferences(self) -> ViewerPreferences | None:
        """The viewer preferences, the async twin of
        ``Document.viewer_preferences``."""

    async def extensions(self) -> list[DeveloperExtension]:
        """The developer extensions, the async twin of
        ``Document.extensions``."""

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

    async def extract_text(self, *, reading_order: str = "content") -> str:
        """Extracts the page's text — the async twin of
        ``Page.extract_text``."""

    async def extract_markdown(self, *, reading_order: str = "content") -> str:
        """Page markdown, ranking heading sizes against that page alone —
        the async twin of ``Page.extract_markdown``.
        ``AsyncDocument.extract_markdown`` is the better answer whenever
        the whole document is at hand."""

    async def spans(self, *, reading_order: str = "content") -> list[Span]:
        """The page's styled text spans — the async twin of
        ``Page.spans``."""

    async def render(
        self,
        scale: float = 1.0,
        fonts: str | None = None,
        font_dir: str | None = None,
        compression: str = "default",
        format: str = "png",
        quality: int = 90,
    ) -> bytes:
        """Renders the page at ``scale`` and resolves to image bytes — the
        async twin of ``Page.render``, with the same arguments and the
        same leniency."""

    async def render_reporting(
        self,
        scale: float = 1.0,
        fonts: str | None = None,
        font_dir: str | None = None,
        compression: str = "default",
        format: str = "png",
        quality: int = 90,
    ) -> tuple[bytes, list[str]]:
        """Renders the page like ``render``, resolving to
        ``(image, warnings)`` — the async twin of ``Page.render_reporting``,
        with the same warning semantics."""

    async def extract_images(self, compression: str = "default") -> list[PageImage]:
        """Every image the page draws, as PNG-encoded ``PageImage``
        entries — the async twin of ``Page.extract_images``, with the
        same drawing-order, native-size and leniency semantics."""

def md_to_pdf(
    markdown: str,
    theme: str | None = None,
    size: str = "a4",
    landscape: bool = False,
    base_dir: str | None = None,
) -> bytes:
    """Composes CommonMark+GFM ``markdown`` into a themed PDF and returns
    the file bytes. ``theme`` is CSS source text, not a path; omitted, the
    built-in default theme applies. ``size`` names a page size
    case-insensitively: ``"a3"``, ``"a4"``, ``"a5"``, ``"letter"`` or
    ``"legal"``, raising ``PdfError`` for anything else. ``base_dir``
    anchors relative image paths and defaults to the current directory.

    Deterministic: the same arguments always produce the same bytes.

    Unencodable characters and skipped raw HTML fragments are replaced
    and reported through a single ``UserWarning`` naming what changed; a
    clean document warns about nothing.
    """

class write:
    """Type stubs for ``pdfboss._pdfboss.write``: composing new PDFs from
    frozen pyclasses accumulated with ``|`` and lowered once at
    ``Pdf.save``/``Pdf.to_bytes`` time.

    Declared as a nested namespace class rather than a stub-package file
    (``_pdfboss/write.pyi``) because pyo3 registers ``write`` as one
    ``PyModule`` object, not a real subpackage on disk, and this single
    ``_pdfboss.pyi`` file is the existing stub layout. Every name below is
    actually an attribute of the real ``pdfboss._pdfboss.write`` module at
    runtime, not of this class; cross-references below are written as
    ``"write.Name"`` rather than a bare name, since a bare nested-class
    name would resolve against this file's top-level (read-side) names —
    ``Page`` above is a different class from ``write.Page`` below.
    """

    @staticmethod
    def merge(inputs: list[bytes | tuple[bytes, list[int]]]) -> bytes:
        """Assembles ``inputs`` into one fresh document: each item is
        either raw bytes (every page) or a ``(bytes, list[int])`` tuple
        selecting specific 0-based pages, gathered in argument order
        under a fresh page tree. An input that cannot be opened at all
        (encrypted with no working password) raises ``PdfError``; one
        that opens under its password, including the empty user
        password, copies across as plain, unencrypted bytes."""

    @staticmethod
    def split(data: bytes, every: int) -> list[bytes]:
        """Cuts ``data`` into consecutive parts of ``every`` pages each,
        the last part carrying whatever remains. A ``data`` that cannot
        be opened at all (encrypted with no working password) raises
        ``PdfError``; one that opens under its password, including the
        empty user password, copies across as plain, unencrypted
        bytes."""

    @staticmethod
    def rotate(
        data: bytes, by: int, pages: list[int] | None = None, rewrite: bool = False
    ) -> bytes:
        """Rotates ``pages`` (0-based; every page when omitted) of
        ``data`` by ``by`` degrees clockwise, restricted to 90, 180 or
        270, else ``ValueError``. Appends an incremental update by
        default; ``rewrite=True`` writes the whole file fresh instead.
        Either mode refuses a page inlined directly into ``/Kids`` with
        no object of its own: pdfboss does not yet restructure such a
        page to rotate it. By default (``rewrite=False``) any encrypted
        ``data`` raises ``PdfError``, whether or not it opens; with
        ``rewrite=True`` only a ``data`` that cannot be opened at all
        raises ``PdfError``. A ``data`` that opens under its password,
        including the empty user password, copies across as plain,
        unencrypted bytes instead."""

    @staticmethod
    def rewrite(data: bytes) -> bytes:
        """Rewrites ``data`` fresh: recompressed, object streams per the
        default options, unreachable objects and earlier update sections
        left behind. A ``data`` that cannot be opened at all (encrypted
        with no working password) raises ``PdfError``; one that opens
        under its password, including the empty user password, copies
        across as plain, unencrypted bytes. An owner-password-only
        file loses its ``/P`` restrictions this way, since the fresh
        output carries no encryption at all."""

    @staticmethod
    def encrypt(
        data: bytes,
        *,
        user_password: str = "",
        owner_password: str = "",
        allow: list[str] | None = None,
    ) -> bytes:
        """AES-256 protects ``data`` under ``user_password`` and/or
        ``owner_password`` (ISO 32000-2 7.6.4.3) and returns the fresh
        encrypted file, restricted by ``allow``: ``"print"``, ``"modify"``,
        ``"copy"``, ``"annotate"``, ``"fill-forms"``, ``"accessibility"``,
        ``"assemble"`` or ``"print-hires"`` (every permission when omitted).
        Raises ``ValueError`` for an unknown ``allow`` value, naming it, or
        when both ``user_password`` and ``owner_password`` are empty."""

    @staticmethod
    def decrypt(data: bytes, *, password: str = "") -> bytes:
        """Removes AES-256 protection from ``data``, opened under
        ``password`` (user or owner password), and returns the fresh plain
        file. A wrong or missing password raises ``PdfError``."""

    @staticmethod
    def watermark(
        data: bytes, overlay: bytes, *, rewrite: bool = False, under: bool = False
    ) -> bytes:
        """Draws the first page of ``overlay`` over every page of ``data``
        and returns the watermarked file. By default that is ``data``'s
        bytes followed by an incremental update, so the original is
        untouched and the result grows by the overlay page's size; with
        ``rewrite=True`` the whole file is written afresh with compression
        and object streams, dropping unreachable objects, which usually
        comes out smaller than ``data``. With ``under=True`` the overlay is
        drawn beneath each page's own content instead of on top of it. By
        default (``rewrite=False``) an encrypted ``data`` or ``overlay``
        raises ``PdfError``, whether or not it opens; with ``rewrite=True``
        only a ``data`` or ``overlay`` that cannot be opened at all raises
        ``PdfError``. One that opens under its password, including the
        empty user password, copies across as plain, unencrypted bytes
        instead."""

    class Standard14:
        """One of the fourteen standard fonts every PDF consumer
        provides."""

        HELVETICA: "write.Standard14"
        HELVETICA_BOLD: "write.Standard14"
        HELVETICA_OBLIQUE: "write.Standard14"
        HELVETICA_BOLD_OBLIQUE: "write.Standard14"
        TIMES_ROMAN: "write.Standard14"
        TIMES_BOLD: "write.Standard14"
        TIMES_ITALIC: "write.Standard14"
        TIMES_BOLD_ITALIC: "write.Standard14"
        COURIER: "write.Standard14"
        COURIER_BOLD: "write.Standard14"
        COURIER_OBLIQUE: "write.Standard14"
        COURIER_BOLD_OBLIQUE: "write.Standard14"
        SYMBOL: "write.Standard14"
        ZAPF_DINGBATS: "write.Standard14"

    class Draw(Protocol):
        """The draw protocol: any object exposing a callable
        ``draw(canvas)`` method composes onto a ``Page`` like ``Text`` or
        ``Image``, painting through the canvas handed to it. The return
        value is ignored, so any return type satisfies this protocol."""

        def draw(self, canvas: "write.Canvas") -> object:
            """Paints onto ``canvas``, in the page's content order."""

    class Text:
        """One line of text at a fixed baseline origin."""

        def __init__(
            self,
            value: str,
            at: tuple[float, float],
            font: "write.Standard14" = ...,
            size: float = 12.0,
            color: tuple[float, float, float] | None = None,
        ) -> None:
            """``font`` defaults to ``Standard14.HELVETICA``. ``color`` is
            an ``(r, g, b)`` tuple in ``[0, 1]``, defaulting to black."""

    class Image:
        """A raster image placed at a point."""

        def __init__(
            self,
            data: str | bytes,
            at: tuple[float, float],
            width: float | None = None,
            height: float | None = None,
        ) -> None:
            """``data`` is a filesystem path or raw image bytes; either
            source is read and decoded at ``save``/``to_bytes`` time.
            Raises ``TypeError`` for any other type. ``width``/``height``
            default to the image's native size in points."""

    class Link:
        """A clickable rectangle, lowered into a link annotation on the
        page."""

        def __init__(
            self,
            rect: tuple[float, float, float, float],
            url: str | None = None,
            page: int | None = None,
        ) -> None:
            """Exactly one of ``url`` or ``page`` must be given; passing
            neither or both raises ``TypeError``."""

    class Paragraph:
        """A block of text wrapped, aligned, and (for
        ``align="justify"``) stretched to fill a rectangle."""

        def __init__(
            self,
            text: str,
            rect: tuple[float, float, float, float],
            font: "write.Standard14" = ...,
            size: float = 11.0,
            leading: float | None = None,
            align: Literal["left", "center", "right", "justify"] = "left",
            color: tuple[float, float, float] | None = None,
        ) -> None:
            """``leading`` defaults to a size-derived line height. Raises
            ``TypeError`` for an unknown ``align``, and ``PdfError`` at
            lowering time if the wrapped text overflows ``rect``.
            ``color`` is an ``(r, g, b)`` tuple in ``[0, 1]``, defaulting
            to black."""

    class Metadata:
        """Document information written to the ``/Info`` dictionary. Dates
        are deferred: the write surface stays clock-free."""

        def __init__(
            self,
            title: str | None = None,
            author: str | None = None,
            subject: str | None = None,
            keywords: str | None = None,
            creator: str | None = None,
            producer: str | None = None,
        ) -> None: ...

    class Bookmark:
        """One outline entry: a title, the page it jumps to, and nested
        children, composed by nesting ``Bookmark`` instances rather than
        ``|``."""

        def __init__(
            self,
            title: str,
            page: int,
            *,
            children: Sequence["write.Bookmark"] = (),
        ) -> None: ...

    class Outline:
        """A document's bookmark panel: an ordered forest of ``Bookmark``
        nodes. A singleton ``Pdf`` slot."""

        def __init__(self, *bookmarks: "write.Bookmark") -> None: ...

    class Attachment:
        """A document-level attachment, embedded via the catalog's
        embedded-files name tree. Carries no dates: the write surface
        stays clock-free."""

        def __init__(
            self,
            name: str,
            data: bytes,
            mime: str | None = None,
            description: str | None = None,
        ) -> None: ...

    class PageLabel:
        """One page-numbering range, taking effect from ``first_page``
        until the next range or the document's end. A singleton-free
        sequence: a ``Pdf`` may carry any number of these."""

        def __init__(
            self,
            first_page: int,
            style: Literal[
                "decimal",
                "roman-upper",
                "roman-lower",
                "letters-upper",
                "letters-lower",
            ]
            | None = None,
            prefix: str | None = None,
            start_at: int = 1,
        ) -> None:
            """Raises ``TypeError`` for an unknown ``style``."""

    class Update:
        """A metadata edit staged over an existing document, serialized
        as an incremental update: the base document's own bytes are
        never rewritten, only appended to.

        Construction only captures a shareable seed of ``doc``; an
        encrypted base is not refused here, only at
        ``save``/``to_bytes`` time, raising ``PdfError``.
        """

        def __init__(self, doc: Document) -> None: ...

        def set_metadata(
            self,
            title: str | None = None,
            author: str | None = None,
            subject: str | None = None,
            keywords: str | None = None,
            creator: str | None = None,
            producer: str | None = None,
        ) -> None:
            """Merges the given fields into the metadata staged for the
            next ``save``/``to_bytes`` call. A field left
            ``None`` keeps whatever an earlier call on this ``Update``
            staged; calling ``set_metadata`` more than once merges
            fields across calls, the latest non-``None`` value winning
            per field."""

        def save(self, path: str | os.PathLike[str]) -> None:
            """Writes the base document's bytes, then an incremental
            update section carrying the staged metadata, to a new file
            at ``path``. Raises ``PdfError`` for an encrypted base, or
            one missing ``/Root`` or a ``startxref`` to chain the
            update against."""

        def to_bytes(self) -> bytes:
            """Like ``save``, but returns the full new file
            bytes instead of writing them to a path. May be called more
            than once."""

    class Viewer:
        """Viewer preferences written to the catalog: initial layout,
        navigation mode, and the page opened at document start. A
        singleton ``Pdf`` slot."""

        def __init__(
            self,
            layout: Literal[
                "single-page",
                "one-column",
                "two-column-left",
                "two-column-right",
                "two-page-left",
                "two-page-right",
            ]
            | None = None,
            mode: Literal["use-none", "use-outlines", "use-thumbs", "full-screen"] | None = None,
            open_to: int | None = None,
        ) -> None:
            """Raises ``TypeError`` for an unknown ``layout`` or ``mode``."""

    class Canvas:
        """The imperative painting surface handed to a draw object's
        ``draw`` method: the page's in-progress canvas, moved in for the
        call's duration and moved back out afterward. Has no public
        constructor — only ``Page``'s draw protocol hands one out. Every
        method raises ``PdfError`` once the canvas has been taken back
        after ``draw`` returns; it must not outlive that call."""

        def text(
            self,
            value: str,
            at: tuple[float, float],
            font: "write.Standard14" = ...,
            size: float = 12.0,
        ) -> None:
            """Shows one line of text with its baseline origin at
            ``at``."""

        def line(
            self, x1: float, y1: float, x2: float, y2: float, width: float = 1.0
        ) -> None:
            """Strokes a straight line from ``(x1, y1)`` to ``(x2, y2)`` at
            ``width``."""

        def rect(self, x: float, y: float, w: float, h: float) -> None:
            """Appends a rectangle subpath."""

        def move_to(self, x: float, y: float) -> None:
            """Begins a new subpath at ``(x, y)``."""

        def line_to(self, x: float, y: float) -> None:
            """Straight segment to ``(x, y)``."""

        def curve_to(
            self, x1: float, y1: float, x2: float, y2: float, x3: float, y3: float
        ) -> None:
            """Cubic Bezier with two control points."""

        def close(self) -> None:
            """Closes the current subpath."""

        def stroke(self) -> None:
            """Strokes the current path."""

        def fill(self) -> None:
            """Fills the current path, nonzero winding."""

        def set_fill(self, rgb: tuple[float, float, float]) -> None:
            """Sets the fill color from an ``(r, g, b)`` tuple."""

        def set_stroke(self, rgb: tuple[float, float, float]) -> None:
            """Sets the stroke color from an ``(r, g, b)`` tuple."""

        def set_line_width(self, width: float) -> None:
            """Sets the stroke line width."""

    class Page:
        """One page: its size and the content composed onto it with
        ``|``."""

        def __init__(self, size: str = "a4", landscape: bool = False) -> None:
            """``size`` names a page size case-insensitively, resolved at
            lowering time."""

        def __or__(
            self,
            rhs: "write.Text | write.Image | write.Link | write.Paragraph "
            "| write.Draw",
        ) -> Self:
            """Composes one more element onto the page: ``Text``,
            ``Image``, ``Link``, ``Paragraph``, or any object with a
            callable ``draw`` attribute (the draw protocol). Returns a new
            ``Page``; the receiver is unchanged. Raises ``TypeError`` for
            an unsupported type."""

    class Pdf:
        """A document under construction: pages in reading order, the
        singleton ``Metadata``/``Outline``/``Viewer`` slots, and the
        ``Attachment``/``PageLabel`` sequences. ``|`` accumulates cheap
        handle copies; nothing is built or read until
        ``save``/``to_bytes``."""

        def __init__(self) -> None: ...

        def __or__(
            self,
            rhs: "write.Page | write.Metadata | write.Outline | write.Viewer "
            "| write.Attachment | write.PageLabel",
        ) -> Self:
            """Composes one more ``Page``, ``Attachment`` or ``PageLabel``
            (each appended), or ``Metadata``/``Outline``/``Viewer`` (each a
            singleton slot) onto the document. Returns a new ``Pdf``; the
            receiver is unchanged. A second ``Metadata``, ``Outline`` or
            ``Viewer`` raises ``TypeError``."""

        def save(self, path: str | os.PathLike[str]) -> None:
            """Serializes and writes the document to ``path``."""

        def to_bytes(self) -> bytes:
            """Serializes the document to file bytes, like ``save``. May
            be called more than once — each call lowers a fresh document
            from the accumulated handles, so the composed value is never
            consumed."""
