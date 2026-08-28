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

use std::path::PathBuf;

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

use pdfboss_core::Point;
use pdfboss_write::{
    Color, Content as CoreContent, Image as CoreImage, ImageData, Link as CoreLink,
    LinkTarget as CoreLinkTarget, Metadata as CoreMetadata, Page as CorePage, Pdf as CorePdf,
    Standard14 as CoreStandard14, Text as CoreText,
};

use crate::{page_size_by_name, pdf_err, PdfError};

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

    /// Composes one more element onto the page: `Text`, `Image` or
    /// `Link`. Returns a new `Page`; the receiver is unchanged.
    fn __or__(&self, rhs: &Bound<'_, PyAny>) -> PyResult<WritePage> {
        let is_element = rhs.downcast::<Text>().is_ok()
            || rhs.downcast::<Image>().is_ok()
            || rhs.downcast::<Link>().is_ok();
        if !is_element {
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

/// Lowers one page-content handle, dispatching by its concrete pyclass.
/// `WritePage::__or__` only ever stores `Text`, `Image` or `Link`
/// handles, so the fallback error is unreachable in practice — it exists
/// so a future bug here is a clear message, not a panic.
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
    Err(PdfError::new_err(format!(
        "unsupported page content: {}",
        item.get_type().name()?
    )))
}

/// A document under construction: pages in reading order, plus at most
/// one `Metadata` slot. `|` accumulates cheap handle copies; nothing is
/// built or read until `save`/`to_bytes`.
#[pyclass(name = "Pdf", module = "pdfboss.write", frozen)]
struct WritePdf {
    pages: Vec<Py<WritePage>>,
    metadata: Option<Py<WriteMetadata>>,
}

#[pymethods]
impl WritePdf {
    #[new]
    fn new() -> WritePdf {
        WritePdf {
            pages: Vec::new(),
            metadata: None,
        }
    }

    /// Composes one more `Page` (appended) or `Metadata` (the singleton
    /// slot) onto the document. Returns a new `Pdf`; the receiver is
    /// unchanged. A second `Metadata` raises `TypeError`.
    fn __or__(&self, rhs: &Bound<'_, PyAny>) -> PyResult<WritePdf> {
        if let Ok(page) = rhs.downcast::<WritePage>() {
            let mut pages = clone_py_vec(rhs.py(), &self.pages);
            pages.push(page.clone().unbind());
            return Ok(WritePdf {
                pages,
                metadata: self.metadata.as_ref().map(|meta| meta.clone_ref(rhs.py())),
            });
        }
        if let Ok(meta) = rhs.downcast::<WriteMetadata>() {
            if self.metadata.is_some() {
                return Err(PyTypeError::new_err(
                    "Pdf already has Metadata — one per document",
                ));
            }
            return Ok(WritePdf {
                pages: clone_py_vec(rhs.py(), &self.pages),
                metadata: Some(meta.clone().unbind()),
            });
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
        py.allow_threads(|| core.save(path)).map_err(pdf_err)
    }

    /// Serializes the document to file bytes, like `save`. May be called
    /// more than once — each call lowers a fresh Rust `Pdf` from the
    /// accumulated handles, so the composed value is never consumed.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let core = self.lower(py)?;
        let bytes = py.allow_threads(|| core.to_bytes()).map_err(pdf_err)?;
        Ok(PyBytes::new(py, &bytes))
    }
}

impl WritePdf {
    fn lower(&self, py: Python<'_>) -> PyResult<CorePdf> {
        let mut pages = Vec::with_capacity(self.pages.len());
        for page in &self.pages {
            pages.push(page.borrow(py).lower(py)?);
        }
        let metadata = self.metadata.as_ref().map(|meta| meta.borrow(py).lower());
        Ok(CorePdf {
            metadata,
            pages,
            ..CorePdf::default()
        })
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
pub(crate) fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let module = PyModule::new(py, "write")?;
    module.add_class::<Standard14>()?;
    module.add_class::<Text>()?;
    module.add_class::<Image>()?;
    module.add_class::<Link>()?;
    module.add_class::<WriteMetadata>()?;
    module.add_class::<WritePage>()?;
    module.add_class::<WritePdf>()?;
    parent.add_submodule(&module)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("pdfboss._pdfboss.write", &module)?;
    Ok(())
}
