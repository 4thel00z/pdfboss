//! Python bindings for pdfboss's write side: `pdfboss.write` composes a
//! PDF from frozen pyclasses accumulated with `|`, each application
//! copying handles cheaply and returning a new value. Composition never
//! touches the Rust document model until `Pdf.save`/`Pdf.to_bytes`, which
//! walk the accumulated pages and slots once, under the GIL, to build a
//! `pdfboss_write::Pdf`, then release the GIL to serialize it.
//!
//! `Pdf`, `Page` and `Metadata` are named `WritePdf`, `WritePage` and
//! `WriteMetadata` in Rust — the plain names collide with the
//! `pdfboss_write` types they lower into — and exposed to Python as
//! `Pdf`, `Page` and `Metadata` via `#[pyclass(name = "...")]`.
//!
//! `Page` also accepts anything exposing a callable `draw` attribute (the
//! draw protocol): `PyDraw` wraps that handle as a `pdfboss_write::Draw`
//! implementor. Its `draw` moves the page's in-progress canvas into a
//! `Canvas` shim, calls the Python object's `draw(canvas)` under a
//! reacquired GIL, then takes the canvas back — the shim errors once
//! taken. A Python exception raised there cannot travel through
//! `pdfboss_write::Error`, so it is stashed in the thread-local
//! `DRAW_ERROR` and re-raised untouched by `WritePdf::save`/`to_bytes`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

use pdfboss_core::{Document as CoreDocument, DocumentSeed, Point};
use pdfboss_write::{
    Attachment as CoreAttachment, Bookmark as CoreBookmark, Canvas as CoreCanvas, Color,
    Content as CoreContent, Draw, Image as CoreImage, ImageData, LabelStyle, Link as CoreLink,
    LinkTarget as CoreLinkTarget, Metadata as CoreMetadata, Outline as CoreOutline,
    Page as CorePage, PageLabel as CorePageLabel, PageLayout, PageMode, Paragraph as CoreParagraph,
    ParagraphAlign, Pdf as CorePdf, Standard14 as CoreStandard14, Text as CoreText,
    Update as CoreUpdate, Viewer as CoreViewer,
};

use crate::{page_size_by_name, pdf_err, Document, PdfError};

std::thread_local! {
    /// Holds a Python exception raised inside a draw-object's `draw()`
    /// call until `draw_error_or` can re-raise it untouched. Set only by
    /// `PyDraw::draw`, read only by `draw_error_or`, on the same thread
    /// within the same `save`/`to_bytes` call — `allow_threads` releases
    /// the GIL without changing threads, so the two always agree.
    static DRAW_ERROR: RefCell<Option<PyErr>> = const { RefCell::new(None) };
}

/// Maps a lowering error from `pdfboss_write`, preferring a Python
/// exception stashed by a failed draw-object call — raised exactly as
/// the Python code raised it — over the generic `PdfError` mapping.
fn draw_error_or(e: pdfboss_write::Error) -> PyErr {
    if let Some(err) = DRAW_ERROR.with(|cell| cell.borrow_mut().take()) {
        return err;
    }
    pdf_err(e)
}

/// Shallow-clones a vector of `Py<T>` handles: cheap reference-count
/// bumps, never a deep copy of the underlying Python object. Shared by
/// every `__or__` that appends to an accumulated `Vec<Py<T>>`.
fn clone_py_vec<T>(py: Python<'_>, items: &[Py<T>]) -> Vec<Py<T>> {
    items.iter().map(|item| item.clone_ref(py)).collect()
}

/// One of the fourteen standard fonts every PDF consumer provides.
#[pyclass(eq, frozen, name = "Standard14", module = "pdfboss.write")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standard14 {
    #[pyo3(name = "HELVETICA")]
    Helvetica,
    #[pyo3(name = "HELVETICA_BOLD")]
    HelveticaBold,
    #[pyo3(name = "HELVETICA_OBLIQUE")]
    HelveticaOblique,
    #[pyo3(name = "HELVETICA_BOLD_OBLIQUE")]
    HelveticaBoldOblique,
    #[pyo3(name = "TIMES_ROMAN")]
    TimesRoman,
    #[pyo3(name = "TIMES_BOLD")]
    TimesBold,
    #[pyo3(name = "TIMES_ITALIC")]
    TimesItalic,
    #[pyo3(name = "TIMES_BOLD_ITALIC")]
    TimesBoldItalic,
    #[pyo3(name = "COURIER")]
    Courier,
    #[pyo3(name = "COURIER_BOLD")]
    CourierBold,
    #[pyo3(name = "COURIER_OBLIQUE")]
    CourierOblique,
    #[pyo3(name = "COURIER_BOLD_OBLIQUE")]
    CourierBoldOblique,
    #[pyo3(name = "SYMBOL")]
    Symbol,
    #[pyo3(name = "ZAPF_DINGBATS")]
    ZapfDingbats,
}

impl From<Standard14> for CoreStandard14 {
    fn from(value: Standard14) -> CoreStandard14 {
        match value {
            Standard14::Helvetica => CoreStandard14::Helvetica,
            Standard14::HelveticaBold => CoreStandard14::HelveticaBold,
            Standard14::HelveticaOblique => CoreStandard14::HelveticaOblique,
            Standard14::HelveticaBoldOblique => CoreStandard14::HelveticaBoldOblique,
            Standard14::TimesRoman => CoreStandard14::TimesRoman,
            Standard14::TimesBold => CoreStandard14::TimesBold,
            Standard14::TimesItalic => CoreStandard14::TimesItalic,
            Standard14::TimesBoldItalic => CoreStandard14::TimesBoldItalic,
            Standard14::Courier => CoreStandard14::Courier,
            Standard14::CourierBold => CoreStandard14::CourierBold,
            Standard14::CourierOblique => CoreStandard14::CourierOblique,
            Standard14::CourierBoldOblique => CoreStandard14::CourierBoldOblique,
            Standard14::Symbol => CoreStandard14::Symbol,
            Standard14::ZapfDingbats => CoreStandard14::ZapfDingbats,
        }
    }
}

/// One line of text at a fixed baseline origin.
#[pyclass(frozen, module = "pdfboss.write")]
struct Text {
    value: String,
    at: (f32, f32),
    font: Standard14,
    size: f32,
    color: Option<(f32, f32, f32)>,
}

#[pymethods]
impl Text {
    #[new]
    #[pyo3(signature = (value, at, font=Standard14::Helvetica, size=12.0, color=None))]
    fn new(
        value: String,
        at: (f32, f32),
        font: Standard14,
        size: f32,
        color: Option<(f32, f32, f32)>,
    ) -> Text {
        Text {
            value,
            at,
            font,
            size,
            color,
        }
    }
}

impl Text {
    /// Lowers to a composed content element; infallible — a `Text`
    /// element carries nothing that can fail before the font actually
    /// encodes it, which happens later, inside `Canvas::text`.
    fn lower(&self) -> CoreContent {
        let color = match self.color {
            Some((r, g, b)) => Color::Rgb(r, g, b),
            None => Color::BLACK,
        };
        CoreContent::Text(CoreText {
            value: self.value.clone(),
            at: Point::new(self.at.0, self.at.1),
            font: self.font.into(),
            size: self.size,
            color,
        })
    }
}

/// Where an `Image`'s pixels come from: a path read at lowering, or raw
/// bytes given directly. No I/O happens at construction time.
enum ImageSource {
    Path(String),
    Bytes(Vec<u8>),
}

/// A raster image placed at a point.
#[pyclass(frozen, module = "pdfboss.write")]
struct Image {
    source: ImageSource,
    at: (f32, f32),
    width: Option<f32>,
    height: Option<f32>,
}

#[pymethods]
impl Image {
    #[new]
    #[pyo3(signature = (data, at, width=None, height=None))]
    fn new(
        data: &Bound<'_, PyAny>,
        at: (f32, f32),
        width: Option<f32>,
        height: Option<f32>,
    ) -> PyResult<Image> {
        let source = if let Ok(path) = data.extract::<String>() {
            ImageSource::Path(path)
        } else if let Ok(bytes) = data.extract::<Vec<u8>>() {
            ImageSource::Bytes(bytes)
        } else {
            return Err(PyTypeError::new_err(format!(
                "Image data must be str (path) or bytes, got {}",
                data.get_type().name()?
            )));
        };
        Ok(Image {
            source,
            at,
            width,
            height,
        })
    }
}

impl Image {
    /// Reads the source bytes (a filesystem read for a path source) and
    /// decodes them, at lowering time only — construction does no I/O.
    fn lower(&self) -> PyResult<CoreContent> {
        let bytes = match &self.source {
            ImageSource::Bytes(data) => data.clone(),
            ImageSource::Path(path) => std::fs::read(path)
                .map_err(|e| PdfError::new_err(format!("failed to read image {path:?}: {e}")))?,
        };
        let data = ImageData::decode(&bytes).map_err(pdf_err)?;
        Ok(CoreContent::Image(CoreImage {
            data,
            at: Point::new(self.at.0, self.at.1),
            width: self.width,
            height: self.height,
        }))
    }
}

/// Where a `Link` leads: exactly one of `url` or `page` is given at
/// construction.
enum LinkTargetValue {
    Uri(String),
    Page(usize),
}

/// A clickable rectangle, lowered into a link annotation on the page.
#[pyclass(frozen, module = "pdfboss.write")]
struct Link {
    rect: (f32, f32, f32, f32),
    target: LinkTargetValue,
}

#[pymethods]
impl Link {
    #[new]
    #[pyo3(signature = (rect, url=None, page=None))]
    fn new(rect: (f32, f32, f32, f32), url: Option<String>, page: Option<usize>) -> PyResult<Link> {
        let target = match (url, page) {
            (Some(url), None) => LinkTargetValue::Uri(url),
            (None, Some(page)) => LinkTargetValue::Page(page),
            (None, None) => {
                return Err(PyTypeError::new_err(
                    "Link needs exactly one of url or page",
                ))
            }
            (Some(_), Some(_)) => {
                return Err(PyTypeError::new_err(
                    "Link accepts only one of url or page, not both",
                ))
            }
        };
        Ok(Link { rect, target })
    }
}

impl Link {
    fn lower(&self) -> CoreContent {
        let target = match &self.target {
            LinkTargetValue::Uri(uri) => CoreLinkTarget::Uri(uri.clone()),
            LinkTargetValue::Page(index) => CoreLinkTarget::Page(*index),
        };
        CoreContent::Link(CoreLink {
            rect: [self.rect.0, self.rect.1, self.rect.2, self.rect.3],
            target,
        })
    }
}

/// Parses a paragraph alignment string, case-sensitive, into the Rust
/// enum — `left`, `center`, `right` or `justify`. Anything else is a
/// `TypeError` naming the valid values, raised at construction so a typo
/// fails fast instead of surfacing deep inside lowering.
fn parse_paragraph_align(value: &str) -> PyResult<ParagraphAlign> {
    match value {
        "left" => Ok(ParagraphAlign::Left),
        "center" => Ok(ParagraphAlign::Center),
        "right" => Ok(ParagraphAlign::Right),
        "justify" => Ok(ParagraphAlign::Justify),
        other => Err(PyTypeError::new_err(format!(
            "unknown paragraph align {other:?}: left, center, right or justify"
        ))),
    }
}

/// A block of text wrapped, aligned, and (for `align="justify"`)
/// stretched to fill a rectangle.
#[pyclass(frozen, module = "pdfboss.write")]
struct Paragraph {
    text: String,
    rect: (f32, f32, f32, f32),
    font: Standard14,
    size: f32,
    leading: Option<f32>,
    align: ParagraphAlign,
    color: Option<(f32, f32, f32)>,
}

#[pymethods]
impl Paragraph {
    #[new]
    #[pyo3(signature = (text, rect, font=Standard14::Helvetica, size=11.0, leading=None, align="left", color=None))]
    fn new(
        text: String,
        rect: (f32, f32, f32, f32),
        font: Standard14,
        size: f32,
        leading: Option<f32>,
        align: &str,
        color: Option<(f32, f32, f32)>,
    ) -> PyResult<Paragraph> {
        let align = parse_paragraph_align(align)?;
        Ok(Paragraph {
            text,
            rect,
            font,
            size,
            leading,
            align,
            color,
        })
    }
}

impl Paragraph {
    fn lower(&self) -> CoreContent {
        let color = match self.color {
            Some((r, g, b)) => Color::Rgb(r, g, b),
            None => Color::BLACK,
        };
        CoreContent::Paragraph(CoreParagraph {
            text: self.text.clone(),
            rect: [self.rect.0, self.rect.1, self.rect.2, self.rect.3],
            font: self.font.into(),
            size: self.size,
            leading: self.leading,
            align: self.align,
            color,
        })
    }
}

/// Document information written to the `/Info` dictionary. Dates are
/// deferred: the write surface stays clock-free, so nothing here reads
/// the current time.
#[pyclass(name = "Metadata", module = "pdfboss.write", frozen)]
struct WriteMetadata {
    title: Option<String>,
    author: Option<String>,
    subject: Option<String>,
    keywords: Option<String>,
    creator: Option<String>,
    producer: Option<String>,
}

#[pymethods]
impl WriteMetadata {
    #[new]
    #[pyo3(signature = (title=None, author=None, subject=None, keywords=None, creator=None, producer=None))]
    fn new(
        title: Option<String>,
        author: Option<String>,
        subject: Option<String>,
        keywords: Option<String>,
        creator: Option<String>,
        producer: Option<String>,
    ) -> WriteMetadata {
        WriteMetadata {
            title,
            author,
            subject,
            keywords,
            creator,
            producer,
        }
    }
}

impl WriteMetadata {
    fn lower(&self) -> CoreMetadata {
        CoreMetadata {
            title: self.title.clone(),
            author: self.author.clone(),
            subject: self.subject.clone(),
            keywords: self.keywords.clone(),
            creator: self.creator.clone(),
            producer: self.producer.clone(),
            creation_date: None,
            modification_date: None,
        }
    }
}

/// One outline entry: a title, the page it jumps to, and nested children,
/// composed by nesting `Bookmark` instances rather than `|`.
#[pyclass(frozen, module = "pdfboss.write")]
struct Bookmark {
    title: String,
    page: usize,
    children: Vec<Py<Bookmark>>,
}

#[pymethods]
impl Bookmark {
    #[new]
    #[pyo3(signature = (title, page, *, children=Vec::new()))]
    fn new(title: String, page: usize, children: Vec<Py<Bookmark>>) -> Bookmark {
        Bookmark {
            title,
            page,
            children,
        }
    }
}

impl Bookmark {
    fn lower(&self, py: Python<'_>) -> CoreBookmark {
        CoreBookmark {
            title: self.title.clone(),
            page: self.page,
            children: self
                .children
                .iter()
                .map(|child| child.borrow(py).lower(py))
                .collect(),
        }
    }
}

/// A document's bookmark panel: an ordered forest of `Bookmark` nodes. A
/// singleton `Pdf` slot.
#[pyclass(frozen, module = "pdfboss.write")]
struct Outline {
    bookmarks: Vec<Py<Bookmark>>,
}

#[pymethods]
impl Outline {
    #[new]
    #[pyo3(signature = (*bookmarks))]
    fn new(bookmarks: Vec<Py<Bookmark>>) -> Outline {
        Outline { bookmarks }
    }
}

impl Outline {
    fn lower(&self, py: Python<'_>) -> CoreOutline {
        CoreOutline {
            bookmarks: self
                .bookmarks
                .iter()
                .map(|bookmark| bookmark.borrow(py).lower(py))
                .collect(),
        }
    }
}

/// A document-level attachment, embedded via the catalog's embedded-files
/// name tree. Attachments carry no dates: the write surface stays
/// clock-free, so there is no `modified` parameter here.
#[pyclass(frozen, module = "pdfboss.write")]
struct Attachment {
    name: String,
    data: Vec<u8>,
    mime: Option<String>,
    description: Option<String>,
}

#[pymethods]
impl Attachment {
    #[new]
    #[pyo3(signature = (name, data, mime=None, description=None))]
    fn new(
        name: String,
        data: Vec<u8>,
        mime: Option<String>,
        description: Option<String>,
    ) -> Attachment {
        Attachment {
            name,
            data,
            mime,
            description,
        }
    }
}

impl Attachment {
    fn lower(&self) -> CoreAttachment {
        CoreAttachment {
            name: self.name.clone(),
            data: self.data.clone(),
            mime: self.mime.clone(),
            modified: None,
            description: self.description.clone(),
        }
    }
}

/// Parses a page-label numbering style, case-sensitive: `decimal`,
/// `roman-upper`, `roman-lower`, `letters-upper` or `letters-lower`.
/// Anything else is a `TypeError` naming the valid values.
fn parse_label_style(value: &str) -> PyResult<LabelStyle> {
    match value {
        "decimal" => Ok(LabelStyle::Decimal),
        "roman-upper" => Ok(LabelStyle::RomanUpper),
        "roman-lower" => Ok(LabelStyle::RomanLower),
        "letters-upper" => Ok(LabelStyle::LettersUpper),
        "letters-lower" => Ok(LabelStyle::LettersLower),
        other => Err(PyTypeError::new_err(format!(
            "unknown page label style {other:?}: decimal, roman-upper, roman-lower, letters-upper or letters-lower"
        ))),
    }
}

/// One page-numbering range, taking effect from `first_page` until the
/// next range or the document's end. A singleton-free sequence: a `Pdf`
/// may carry any number of these.
#[pyclass(frozen, module = "pdfboss.write")]
struct PageLabel {
    first_page: usize,
    style: Option<LabelStyle>,
    prefix: Option<String>,
    start_at: u32,
}

#[pymethods]
impl PageLabel {
    #[new]
    #[pyo3(signature = (first_page, style=None, prefix=None, start_at=1))]
    fn new(
        first_page: usize,
        style: Option<&str>,
        prefix: Option<String>,
        start_at: u32,
    ) -> PyResult<PageLabel> {
        let style = style.map(parse_label_style).transpose()?;
        Ok(PageLabel {
            first_page,
            style,
            prefix,
            start_at,
        })
    }
}

impl PageLabel {
    fn lower(&self) -> CorePageLabel {
        CorePageLabel {
            first_page: self.first_page,
            style: self.style,
            prefix: self.prefix.clone(),
            start_at: self.start_at,
        }
    }
}

/// Parses an initial page-layout mode, case-sensitive, kebab-cased from
/// the Rust `PageLayout` variant names. Anything else is a `TypeError`
/// naming the valid values.
fn parse_page_layout(value: &str) -> PyResult<PageLayout> {
    match value {
        "single-page" => Ok(PageLayout::SinglePage),
        "one-column" => Ok(PageLayout::OneColumn),
        "two-column-left" => Ok(PageLayout::TwoColumnLeft),
        "two-column-right" => Ok(PageLayout::TwoColumnRight),
        "two-page-left" => Ok(PageLayout::TwoPageLeft),
        "two-page-right" => Ok(PageLayout::TwoPageRight),
        other => Err(PyTypeError::new_err(format!(
            "unknown page layout {other:?}: single-page, one-column, two-column-left, two-column-right, two-page-left or two-page-right"
        ))),
    }
}

/// Parses an initial navigation-panel mode, case-sensitive, kebab-cased
/// from the Rust `PageMode` variant names. Anything else is a `TypeError`
/// naming the valid values.
fn parse_page_mode(value: &str) -> PyResult<PageMode> {
    match value {
        "use-none" => Ok(PageMode::UseNone),
        "use-outlines" => Ok(PageMode::UseOutlines),
        "use-thumbs" => Ok(PageMode::UseThumbs),
        "full-screen" => Ok(PageMode::FullScreen),
        other => Err(PyTypeError::new_err(format!(
            "unknown page mode {other:?}: use-none, use-outlines, use-thumbs or full-screen"
        ))),
    }
}

/// Viewer preferences written to the catalog: initial layout, navigation
/// mode, and the page opened at document start. A singleton `Pdf` slot.
#[pyclass(frozen, module = "pdfboss.write")]
struct Viewer {
    layout: Option<PageLayout>,
    mode: Option<PageMode>,
    open_to: Option<usize>,
}

#[pymethods]
impl Viewer {
    #[new]
    #[pyo3(signature = (layout=None, mode=None, open_to=None))]
    fn new(layout: Option<&str>, mode: Option<&str>, open_to: Option<usize>) -> PyResult<Viewer> {
        let layout = layout.map(parse_page_layout).transpose()?;
        let mode = mode.map(parse_page_mode).transpose()?;
        Ok(Viewer {
            layout,
            mode,
            open_to,
        })
    }
}

impl Viewer {
    fn lower(&self) -> CoreViewer {
        CoreViewer {
            layout: self.layout,
            mode: self.mode,
            open_to: self.open_to,
        }
    }
}

/// The message a `Canvas` shim method raises once its interior canvas has
/// already been taken back — after the `draw()` call that received it has
/// returned.
const CANVAS_TAKEN: &str = "canvas is no longer usable outside draw()";

/// The imperative painting surface handed to a draw object's `draw`
/// method: the page's in-progress `pdfboss_write::Canvas` is moved into
/// this shim for the call's duration and moved back out afterward, so
/// painting through it lands in the same canvas as every other element
/// on the page, in content order. Every method locks the interior and
/// errors with `PdfError` if the canvas has already been taken back —
/// the value must not outlive its `draw` call.
#[pyclass(frozen, module = "pdfboss.write")]
struct Canvas {
    inner: Mutex<Option<CoreCanvas>>,
}

impl Canvas {
    fn new(canvas: CoreCanvas) -> Canvas {
        Canvas {
            inner: Mutex::new(Some(canvas)),
        }
    }

    /// Locks the interior canvas and runs `f` on it, or raises `PdfError`
    /// when the canvas has already been taken back.
    fn with_canvas<R>(&self, f: impl FnOnce(&mut CoreCanvas) -> R) -> PyResult<R> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let canvas = guard
            .as_mut()
            .ok_or_else(|| PdfError::new_err(CANVAS_TAKEN))?;
        Ok(f(canvas))
    }
}

#[pymethods]
impl Canvas {
    /// Shows one line of text with its baseline origin at `at`.
    #[pyo3(signature = (value, at, font=Standard14::Helvetica, size=12.0))]
    fn text(&self, value: &str, at: (f32, f32), font: Standard14, size: f32) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.text(value, at.0, at.1, font.into(), size))?
            .map_err(pdf_err)
    }

    /// Strokes a straight line from `(x1, y1)` to `(x2, y2)` at `width`.
    #[pyo3(signature = (x1, y1, x2, y2, width=1.0))]
    fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32) -> PyResult<()> {
        self.with_canvas(|canvas| {
            canvas.set_line_width(width);
            canvas.move_to(x1, y1);
            canvas.line_to(x2, y2);
            canvas.stroke();
        })
    }

    /// Appends a rectangle subpath.
    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.rect(x, y, w, h))
    }

    /// Begins a new subpath at `(x, y)`.
    fn move_to(&self, x: f32, y: f32) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.move_to(x, y))
    }

    /// Straight segment to `(x, y)`.
    fn line_to(&self, x: f32, y: f32) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.line_to(x, y))
    }

    /// Cubic Bézier with two control points.
    #[allow(clippy::too_many_arguments)] // six coordinates is the operator's arity
    fn curve_to(&self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.curve_to(x1, y1, x2, y2, x3, y3))
    }

    /// Closes the current subpath.
    fn close(&self) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.close())
    }

    /// Strokes the current path.
    fn stroke(&self) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.stroke())
    }

    /// Fills the current path, nonzero winding.
    fn fill(&self) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.fill())
    }

    /// Sets the fill color from an `(r, g, b)` tuple.
    fn set_fill(&self, rgb: (f32, f32, f32)) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.set_fill(Color::Rgb(rgb.0, rgb.1, rgb.2)))
    }

    /// Sets the stroke color from an `(r, g, b)` tuple.
    fn set_stroke(&self, rgb: (f32, f32, f32)) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.set_stroke(Color::Rgb(rgb.0, rgb.1, rgb.2)))
    }

    /// Sets the stroke line width.
    fn set_line_width(&self, width: f32) -> PyResult<()> {
        self.with_canvas(|canvas| canvas.set_line_width(width))
    }
}

/// Wraps a Python object exposing a callable `draw` attribute as a Rust
/// `Draw` implementor, so it can sit in a page's content alongside
/// `Text`/`Image`/`Link`/`Paragraph` and paint in the same position.
struct PyDraw {
    obj: Py<PyAny>,
}

impl PyDraw {
    fn new(obj: &Bound<'_, PyAny>) -> PyDraw {
        PyDraw {
            obj: obj.clone().unbind(),
        }
    }
}

impl Draw for PyDraw {
    /// Moves `canvas` into a fresh `Canvas`, calls the wrapped object's
    /// `draw(canvas)` under a reacquired GIL, and reclaims the
    /// (possibly painted-on) canvas back into `*canvas` — unconditionally,
    /// whether `draw` raised or not, so a shim the Python object smuggled
    /// out (say, stashed on `self` before raising) is left holding no
    /// canvas either way and errors on any further use. Only when the
    /// shim itself could never be created (`Py::new` failing) is there
    /// nothing to reclaim. A raised Python exception is stashed in
    /// `DRAW_ERROR` for `draw_error_or` to re-raise untouched; this call
    /// returns a sentinel `pdfboss_write::Error` instead, since that type
    /// cannot carry a `PyErr`.
    fn draw(&self, canvas: &mut CoreCanvas) -> pdfboss_write::Result<()> {
        let taken = std::mem::take(canvas);
        let outcome = Python::with_gil(|py| -> PyResult<(CoreCanvas, Option<PyErr>)> {
            let shim = Py::new(py, Canvas::new(taken))?;
            let draw_result = self
                .obj
                .bind(py)
                .call_method1("draw", (shim.clone_ref(py),));
            let shim_ref = shim.borrow(py);
            let mut guard = shim_ref
                .inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let restored = guard
                .take()
                .ok_or_else(|| PdfError::new_err("canvas shim lost its canvas unexpectedly"))?;
            Ok((restored, draw_result.err()))
        });
        match outcome {
            Ok((restored, None)) => {
                *canvas = restored;
                Ok(())
            }
            Ok((restored, Some(err))) => {
                *canvas = restored;
                DRAW_ERROR.with(|cell| *cell.borrow_mut() = Some(err));
                Err(pdfboss_write::Error::Other(
                    "python draw() raised an exception".to_string(),
                ))
            }
            Err(err) => {
                DRAW_ERROR.with(|cell| *cell.borrow_mut() = Some(err));
                Err(pdfboss_write::Error::Other(
                    "python draw() raised an exception".to_string(),
                ))
            }
        }
    }
}

/// True when `obj` has an attribute named `draw` that is itself callable
/// — the draw protocol's entire contract. Neither `hasattr` nor the
/// callable check alone is enough: a non-callable `draw` attribute is not
/// a draw object.
fn has_callable_draw(obj: &Bound<'_, PyAny>) -> bool {
    obj.getattr("draw")
        .map(|attr| attr.is_callable())
        .unwrap_or(false)
}

/// One page: its size and the content composed onto it with `|`. `size`
/// is resolved case-insensitively at lowering, via the same
/// `page_size_by_name` the markdown composer uses.
#[pyclass(name = "Page", module = "pdfboss.write", frozen)]
struct WritePage {
    size: String,
    landscape: bool,
    content: Vec<Py<PyAny>>,
}

#[pymethods]
impl WritePage {
    #[new]
    #[pyo3(signature = (size="a4", landscape=false))]
    fn new(size: &str, landscape: bool) -> WritePage {
        WritePage {
            size: size.to_string(),
            landscape,
            content: Vec::new(),
        }
    }

    /// Composes one more element onto the page: `Text`, `Image`, `Link`,
    /// `Paragraph`, or any object with a callable `draw` attribute (the
    /// draw protocol). Returns a new `Page`; the receiver is unchanged.
    fn __or__(&self, rhs: &Bound<'_, PyAny>) -> PyResult<WritePage> {
        let is_element = rhs.downcast::<Text>().is_ok()
            || rhs.downcast::<Image>().is_ok()
            || rhs.downcast::<Link>().is_ok()
            || rhs.downcast::<Paragraph>().is_ok();
        if !is_element && !has_callable_draw(rhs) {
            return Err(PyTypeError::new_err(format!(
                "Page cannot compose with {}",
                rhs.get_type().name()?
            )));
        }
        let mut content = clone_py_vec(rhs.py(), &self.content);
        content.push(rhs.clone().unbind());
        Ok(WritePage {
            size: self.size.clone(),
            landscape: self.landscape,
            content,
        })
    }
}

impl WritePage {
    fn lower(&self, py: Python<'_>) -> PyResult<CorePage> {
        let size = page_size_by_name(&self.size)?;
        let size = if self.landscape {
            size.landscape()
        } else {
            size
        };
        let mut content = Vec::with_capacity(self.content.len());
        for item in &self.content {
            content.push(lower_content(item.bind(py))?);
        }
        Ok(CorePage {
            size,
            content,
            ..CorePage::default()
        })
    }
}

/// Lowers one page-content handle, dispatching by its concrete pyclass,
/// with a callable `draw` attribute as the catch-all for anything else.
/// `WritePage::__or__` only ever stores handles matching one of these, so
/// the fallback error is unreachable in practice — it exists so a future
/// bug here is a clear message, not a panic.
fn lower_content(item: &Bound<'_, PyAny>) -> PyResult<CoreContent> {
    if let Ok(text) = item.downcast::<Text>() {
        return Ok(text.borrow().lower());
    }
    if let Ok(image) = item.downcast::<Image>() {
        return image.borrow().lower();
    }
    if let Ok(link) = item.downcast::<Link>() {
        return Ok(link.borrow().lower());
    }
    if let Ok(paragraph) = item.downcast::<Paragraph>() {
        return Ok(paragraph.borrow().lower());
    }
    if has_callable_draw(item) {
        return Ok(CoreContent::custom(PyDraw::new(item)));
    }
    Err(PdfError::new_err(format!(
        "unsupported page content: {}",
        item.get_type().name()?
    )))
}

/// A document under construction: pages in reading order, the singleton
/// `Metadata`/`Outline`/`Viewer` slots, and the `Attachment`/`PageLabel`
/// sequences. `|` accumulates cheap handle copies; nothing is built or
/// read until `save`/`to_bytes`.
#[pyclass(name = "Pdf", module = "pdfboss.write", frozen)]
struct WritePdf {
    pages: Vec<Py<WritePage>>,
    metadata: Option<Py<WriteMetadata>>,
    outline: Option<Py<Outline>>,
    attachments: Vec<Py<Attachment>>,
    page_labels: Vec<Py<PageLabel>>,
    viewer: Option<Py<Viewer>>,
}

#[pymethods]
impl WritePdf {
    #[new]
    fn new() -> WritePdf {
        WritePdf {
            pages: Vec::new(),
            metadata: None,
            outline: None,
            attachments: Vec::new(),
            page_labels: Vec::new(),
            viewer: None,
        }
    }

    /// Composes one more `Page` or `Attachment` or `PageLabel` (each
    /// appended), or `Metadata`/`Outline`/`Viewer` (each a singleton slot)
    /// onto the document. Returns a new `Pdf`; the receiver is unchanged.
    /// A second `Metadata`, `Outline` or `Viewer` raises `TypeError`.
    fn __or__(&self, rhs: &Bound<'_, PyAny>) -> PyResult<WritePdf> {
        let py = rhs.py();
        if let Ok(page) = rhs.downcast::<WritePage>() {
            let mut next = self.shallow_clone(py);
            next.pages.push(page.clone().unbind());
            return Ok(next);
        }
        if let Ok(meta) = rhs.downcast::<WriteMetadata>() {
            if self.metadata.is_some() {
                return Err(PyTypeError::new_err(
                    "Pdf already has Metadata — one per document",
                ));
            }
            let mut next = self.shallow_clone(py);
            next.metadata = Some(meta.clone().unbind());
            return Ok(next);
        }
        if let Ok(outline) = rhs.downcast::<Outline>() {
            if self.outline.is_some() {
                return Err(PyTypeError::new_err(
                    "Pdf already has Outline — one per document",
                ));
            }
            let mut next = self.shallow_clone(py);
            next.outline = Some(outline.clone().unbind());
            return Ok(next);
        }
        if let Ok(viewer) = rhs.downcast::<Viewer>() {
            if self.viewer.is_some() {
                return Err(PyTypeError::new_err(
                    "Pdf already has Viewer — one per document",
                ));
            }
            let mut next = self.shallow_clone(py);
            next.viewer = Some(viewer.clone().unbind());
            return Ok(next);
        }
        if let Ok(attachment) = rhs.downcast::<Attachment>() {
            let mut next = self.shallow_clone(py);
            next.attachments.push(attachment.clone().unbind());
            return Ok(next);
        }
        if let Ok(label) = rhs.downcast::<PageLabel>() {
            let mut next = self.shallow_clone(py);
            next.page_labels.push(label.clone().unbind());
            return Ok(next);
        }
        Err(PyTypeError::new_err(format!(
            "Pdf cannot compose with {}",
            rhs.get_type().name()?
        )))
    }

    /// Serializes and writes the document to `path`. Lowers the
    /// accumulated pages and slots into a Rust `Pdf` under the GIL, then
    /// releases it for the actual serialization.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        let core = self.lower(py)?;
        DRAW_ERROR.with(|cell| cell.borrow_mut().take());
        py.allow_threads(|| core.save(path)).map_err(draw_error_or)
    }

    /// Serializes the document to file bytes, like `save`. May be called
    /// more than once — each call lowers a fresh Rust `Pdf` from the
    /// accumulated handles, so the composed value is never consumed.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let core = self.lower(py)?;
        DRAW_ERROR.with(|cell| cell.borrow_mut().take());
        let bytes = py
            .allow_threads(|| core.to_bytes())
            .map_err(draw_error_or)?;
        Ok(PyBytes::new(py, &bytes))
    }
}

impl WritePdf {
    /// Copies every field as cheap handle clones — the shared first step
    /// of every `__or__` branch, which then overwrites or appends to
    /// exactly one field.
    fn shallow_clone(&self, py: Python<'_>) -> WritePdf {
        WritePdf {
            pages: clone_py_vec(py, &self.pages),
            metadata: self.metadata.as_ref().map(|meta| meta.clone_ref(py)),
            outline: self.outline.as_ref().map(|outline| outline.clone_ref(py)),
            attachments: clone_py_vec(py, &self.attachments),
            page_labels: clone_py_vec(py, &self.page_labels),
            viewer: self.viewer.as_ref().map(|viewer| viewer.clone_ref(py)),
        }
    }

    fn lower(&self, py: Python<'_>) -> PyResult<CorePdf> {
        let mut pages = Vec::with_capacity(self.pages.len());
        for page in &self.pages {
            pages.push(page.borrow(py).lower(py)?);
        }
        let metadata = self.metadata.as_ref().map(|meta| meta.borrow(py).lower());
        let outline = self
            .outline
            .as_ref()
            .map(|outline| outline.borrow(py).lower(py));
        let attachments = self
            .attachments
            .iter()
            .map(|attachment| attachment.borrow(py).lower())
            .collect();
        let page_labels = self
            .page_labels
            .iter()
            .map(|label| label.borrow(py).lower())
            .collect();
        let viewer = self.viewer.as_ref().map(|viewer| viewer.borrow(py).lower());
        Ok(CorePdf {
            metadata,
            pages,
            outline,
            attachments,
            page_labels,
            viewer,
            ..CorePdf::default()
        })
    }
}

/// A metadata edit staged over an existing document, serialized as an
/// incremental update (ISO 32000-1 §7.5.6): the base document's own bytes
/// are never rewritten, only appended to.
///
/// Holds a [`DocumentSeed`] taken from the given [`Document`] at
/// construction, not the document itself, so `save`/`to_bytes`
/// rebuild a private core document and run under `py.allow_threads`
/// without contending on the shared one. Construction never reads the
/// base's `/Encrypt` entry: a document with an encrypted base is only
/// refused once `save`/`to_bytes` actually opens a
/// `pdfboss_write::Update` on it, raising `PdfError`.
///
/// `set_metadata` may be called more than once before saving: each call
/// merges its given fields into the metadata staged for this `Update`,
/// a field passed as `None` keeping whatever an earlier call staged
/// (the class as a whole, not just the base document's own `/Info`
/// values). The Rust `Update::set_metadata` layer then merges that
/// staged value against the base document's own `/Info` dictionary the
/// same way: a field still `None` after every `set_metadata` call keeps
/// the base's existing value rather than clearing it.
#[pyclass(name = "Update", module = "pdfboss.write", frozen)]
struct WriteUpdate {
    seed: DocumentSeed,
    meta: Mutex<CoreMetadata>,
}

#[pymethods]
impl WriteUpdate {
    #[new]
    fn new(doc: PyRef<'_, Document>) -> WriteUpdate {
        WriteUpdate {
            seed: doc.seed(),
            meta: Mutex::new(CoreMetadata::default()),
        }
    }

    /// Merges the given fields into the metadata staged for the next
    /// `save`/`to_bytes` call. A field left `None` keeps
    /// whatever an earlier `set_metadata` call on this `Update` staged.
    #[pyo3(signature = (title=None, author=None, subject=None, keywords=None, creator=None, producer=None))]
    #[allow(clippy::too_many_arguments)]
    fn set_metadata(
        &self,
        title: Option<String>,
        author: Option<String>,
        subject: Option<String>,
        keywords: Option<String>,
        creator: Option<String>,
        producer: Option<String>,
    ) {
        let mut meta = self.meta.lock().unwrap_or_else(PoisonError::into_inner);
        merge_metadata_field(&mut meta.title, title);
        merge_metadata_field(&mut meta.author, author);
        merge_metadata_field(&mut meta.subject, subject);
        merge_metadata_field(&mut meta.keywords, keywords);
        merge_metadata_field(&mut meta.creator, creator);
        merge_metadata_field(&mut meta.producer, producer);
    }

    /// Writes the base document's bytes, then an incremental update
    /// section carrying the staged metadata, to a new file at `path`.
    /// Raises `PdfError` for an encrypted base, or one missing `/Root`
    /// or a `startxref` to chain the update against.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        let seed = self.seed.clone();
        let meta = self
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        py.allow_threads(move || {
            let doc = CoreDocument::from_seed(seed);
            let mut update = CoreUpdate::new(&doc).map_err(pdf_err)?;
            update.set_metadata(meta).map_err(pdf_err)?;
            update.save(path).map_err(pdf_err)
        })
    }

    /// Like `save`, but returns the full new file bytes (the
    /// base's own bytes followed by the update section) instead of
    /// writing them to a path. May be called more than once.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let seed = self.seed.clone();
        let meta = self
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let bytes = py.allow_threads(move || {
            let doc = CoreDocument::from_seed(seed);
            let mut update = CoreUpdate::new(&doc).map_err(pdf_err)?;
            update.set_metadata(meta).map_err(pdf_err)?;
            update.bytes().map_err(pdf_err)
        })?;
        Ok(PyBytes::new(py, &bytes))
    }
}

/// Overwrites `field` with `value` when `value` is `Some`; leaves it
/// untouched otherwise. The merge rule behind `WriteUpdate::set_metadata`:
/// later `Some` wins, per field, across every call on one `Update`.
fn merge_metadata_field(field: &mut Option<String>, value: Option<String>) {
    if value.is_some() {
        *field = value;
    }
}

/// Builds the `write` submodule, registers its classes, and makes it
/// importable as `pdfboss._pdfboss.write`.
///
/// `PyModule::new` + `add_submodule` alone only exposes the submodule as
/// an *attribute* of `_pdfboss` (`_pdfboss.write.Pdf` works via attribute
/// access); `import pdfboss._pdfboss.write` or `from
/// pdfboss._pdfboss.write import Pdf` additionally need the submodule
/// registered in `sys.modules` under its dotted name.
/// Draws the first page of `overlay` over every page of `data`, returning
/// the watermarked file. By default that is `data`'s bytes followed by an
/// incremental update that adds the overlay page as a form and rewrites
/// each page to draw it; with `rewrite=True` the whole file is written
/// afresh through the writer with compression and object streams, which
/// leaves unreachable objects behind and usually comes out smaller than
/// `data`. Releases the GIL while both files are parsed and the result is
/// built.
#[pyfunction]
#[pyo3(signature = (data, overlay, *, rewrite=false))]
fn watermark<'py>(
    py: Python<'py>,
    data: Vec<u8>,
    overlay: Vec<u8>,
    rewrite: bool,
) -> PyResult<Bound<'py, PyBytes>> {
    let bytes = py.allow_threads(|| {
        let base = pdfboss_core::Document::load(data).map_err(crate::pdf_err)?;
        let overlay = pdfboss_core::Document::load(overlay).map_err(crate::pdf_err)?;
        if rewrite {
            return pdfboss_write::watermark_with(
                &base,
                &overlay,
                pdfboss_write::WriteOptions::default(),
            )
            .map_err(crate::pdf_err);
        }
        pdfboss_write::watermark(&base, &overlay).map_err(crate::pdf_err)
    })?;
    Ok(PyBytes::new(py, &bytes))
}

pub(crate) fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let module = PyModule::new(py, "write")?;
    module.add_function(wrap_pyfunction!(watermark, &module)?)?;
    module.add_class::<Standard14>()?;
    module.add_class::<Text>()?;
    module.add_class::<Image>()?;
    module.add_class::<Link>()?;
    module.add_class::<Paragraph>()?;
    module.add_class::<Bookmark>()?;
    module.add_class::<Outline>()?;
    module.add_class::<Attachment>()?;
    module.add_class::<PageLabel>()?;
    module.add_class::<Viewer>()?;
    module.add_class::<Canvas>()?;
    module.add_class::<WriteMetadata>()?;
    module.add_class::<WritePage>()?;
    module.add_class::<WritePdf>()?;
    module.add_class::<WriteUpdate>()?;
    parent.add_submodule(&module)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("pdfboss._pdfboss.write", &module)?;
    Ok(())
}
