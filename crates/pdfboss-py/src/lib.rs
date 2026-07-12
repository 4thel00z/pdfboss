//! Python bindings for pdfboss, compiled as the extension module
//! `pdfboss._pdfboss` and re-exported by the `pdfboss` package shim.
//!
//! `Document` and `Page` are frozen pyclasses that share the parsed
//! document through an [`Arc`], so they may be used from any Python thread.
//! Access to the underlying document model is serialized by an internal
//! lock, and text extraction and rendering release the GIL while the
//! CPU-bound work runs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use pdfboss_core::Document as CoreDocument;
use pdfboss_core::Page as CorePage;

create_exception!(
    pdfboss,
    PdfError,
    PyException,
    "Raised for any PDF processing error (bad data, encryption, decode failures, I/O)."
);

/// Maps any error from the Rust crates to [`PdfError`] with its display text.
fn pdf_err(e: impl std::fmt::Display) -> PyErr {
    PdfError::new_err(e.to_string())
}

/// Formats a `(major, minor)` header version as `"major.minor"`.
fn version_string(version: (u8, u8)) -> String {
    format!("{}.{}", version.0, version.1)
}

/// Normalizes a possibly-negative sequence index against `count`.
/// Returns `None` when the index is out of range.
fn normalize_index(index: isize, count: usize) -> Option<usize> {
    let count = isize::try_from(count).ok()?;
    let idx = if index < 0 { index + count } else { index };
    if (0..count).contains(&idx) {
        Some(idx as usize)
    } else {
        None
    }
}

/// The core document behind a lock, shareable across threads.
///
/// [`CoreDocument`] itself is neither `Send` nor `Sync`: its interior
/// object cache uses `RefCell`s holding reference-counted entries. That
/// state is fully encapsulated — no reference-counted pointer or `RefCell`
/// borrow ever escapes the core API (cached objects are handed out as deep
/// clones) — so every touch of it happens inside a method call on the
/// wrapped value, and the [`Mutex`] serializes those calls.
struct SharedDocument(Mutex<CoreDocument>);

// SAFETY: see the type-level comment — the non-thread-safe interior state
// never escapes `CoreDocument`'s API, and the `Mutex` serializes all access
// to it, so moving or sharing the wrapper between threads is sound.
unsafe impl Send for SharedDocument {}
unsafe impl Sync for SharedDocument {}

impl SharedDocument {
    fn new(core: CoreDocument) -> Arc<Self> {
        Arc::new(SharedDocument(Mutex::new(core)))
    }

    /// Locks the document. A poisoned lock is recovered: the interior
    /// state is a plain object cache with no cross-call invariants.
    fn lock(&self) -> MutexGuard<'_, CoreDocument> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A loaded PDF document.
#[pyclass(frozen)]
struct Document {
    inner: Arc<SharedDocument>,
}

#[pymethods]
impl Document {
    #[new]
    #[pyo3(signature = (path=None, *, data=None))]
    fn new(path: Option<PathBuf>, data: Option<Vec<u8>>) -> PyResult<Self> {
        let core = match (path, data) {
            (Some(p), None) => CoreDocument::open(p).map_err(pdf_err)?,
            (None, Some(d)) => CoreDocument::load(d).map_err(pdf_err)?,
            _ => {
                return Err(PyValueError::new_err(
                    "Document() takes exactly one of `path` or `data`",
                ))
            }
        };
        Ok(Document {
            inner: SharedDocument::new(core),
        })
    }

    /// Number of pages in the document.
    #[getter]
    fn page_count(&self) -> usize {
        self.inner.lock().page_count()
    }

    /// PDF version from the file header, e.g. "1.7".
    #[getter]
    fn version(&self) -> String {
        version_string(self.inner.lock().version())
    }

    /// Document metadata; only keys present in the file are included.
    #[getter]
    fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let meta = self.inner.lock().metadata();
        let dict = PyDict::new(py);
        let entries = [
            ("title", meta.title),
            ("author", meta.author),
            ("subject", meta.subject),
            ("keywords", meta.keywords),
            ("creator", meta.creator),
            ("producer", meta.producer),
            ("creation_date", meta.creation_date),
            ("mod_date", meta.mod_date),
        ];
        for (key, value) in entries {
            if let Some(value) = value {
                dict.set_item(key, value)?;
            }
        }
        Ok(dict)
    }

    fn __len__(&self) -> usize {
        self.inner.lock().page_count()
    }

    fn __getitem__(&self, index: &Bound<'_, PyAny>) -> PyResult<Page> {
        // Accept an arbitrary Python object so that an index too large to
        // fit in `isize` surfaces as IndexError (the documented behavior)
        // rather than the OverflowError pyo3 would raise while coercing.
        let page = {
            let doc = self.inner.lock();
            let count = doc.page_count();
            let idx = index
                .extract::<isize>()
                .ok()
                .and_then(|i| normalize_index(i, count));
            let Some(idx) = idx else {
                return Err(PyIndexError::new_err(format!(
                    "page index {index} out of range ({count} pages)"
                )));
            };
            doc.page(idx).map_err(pdf_err)?
        };
        Ok(Page {
            doc: Arc::clone(&self.inner),
            page,
        })
    }

    /// Extracts text from all pages, joined by form feed ("\f").
    /// Releases the GIL while the extraction runs.
    fn extract_text(&self, py: Python<'_>) -> PyResult<String> {
        let inner = &self.inner;
        py.allow_threads(move || {
            let doc = inner.lock();
            let mut out = String::new();
            for i in 0..doc.page_count() {
                if i > 0 {
                    out.push('\u{c}');
                }
                let page = doc.page(i).map_err(pdf_err)?;
                let text = pdfboss_text::extract_text(&doc, &page).map_err(pdf_err)?;
                out.push_str(&text);
            }
            Ok(out)
        })
    }
}

/// A single page of a document.
#[pyclass(frozen)]
struct Page {
    doc: Arc<SharedDocument>,
    page: CorePage,
}

#[pymethods]
impl Page {
    /// 0-based page index.
    #[getter]
    fn number(&self) -> usize {
        self.page.index
    }

    /// Page width in points (after rotation).
    #[getter]
    fn width(&self) -> f32 {
        self.page.size().0
    }

    /// Page height in points (after rotation).
    #[getter]
    fn height(&self) -> f32 {
        self.page.size().1
    }

    /// Page rotation in degrees: 0, 90, 180 or 270.
    #[getter]
    fn rotation(&self) -> i32 {
        self.page.rotate
    }

    /// Extracts the page's text. Releases the GIL while the extraction runs.
    fn extract_text(&self, py: Python<'_>) -> PyResult<String> {
        py.allow_threads(|| {
            let doc = self.doc.lock();
            pdfboss_text::extract_text(&doc, &self.page).map_err(pdf_err)
        })
    }

    /// Renders the page and returns PNG bytes. Releases the GIL while the
    /// rasterization and PNG encoding run.
    #[pyo3(signature = (scale=1.0))]
    fn render<'py>(&self, py: Python<'py>, scale: f32) -> PyResult<Bound<'py, PyBytes>> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(PyValueError::new_err(
                "scale must be a positive, finite number",
            ));
        }
        let png = py.allow_threads(|| {
            let doc = self.doc.lock();
            let pixmap = pdfboss_render::render_page(&doc, &self.page, scale).map_err(pdf_err)?;
            pixmap.encode_png().map_err(pdf_err)
        })?;
        Ok(PyBytes::new(py, &png))
    }
}

#[pymodule]
fn _pdfboss(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("PdfError", m.py().get_type::<PdfError>())?;
    m.add_class::<Document>()?;
    m.add_class::<Page>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{normalize_index, version_string};

    #[test]
    fn version_string_formats_major_minor() {
        assert_eq!(version_string((1, 7)), "1.7");
        assert_eq!(version_string((2, 0)), "2.0");
    }

    #[test]
    fn normalize_index_positive_in_range() {
        assert_eq!(normalize_index(0, 3), Some(0));
        assert_eq!(normalize_index(2, 3), Some(2));
    }

    #[test]
    fn normalize_index_negative_in_range() {
        assert_eq!(normalize_index(-1, 3), Some(2));
        assert_eq!(normalize_index(-3, 3), Some(0));
    }

    #[test]
    fn normalize_index_out_of_range() {
        assert_eq!(normalize_index(3, 3), None);
        assert_eq!(normalize_index(-4, 3), None);
        assert_eq!(normalize_index(5, 1), None);
    }

    #[test]
    fn normalize_index_empty() {
        assert_eq!(normalize_index(0, 0), None);
        assert_eq!(normalize_index(-1, 0), None);
    }

    #[test]
    fn normalize_index_extremes() {
        assert_eq!(normalize_index(isize::MAX, 3), None);
        assert_eq!(normalize_index(isize::MIN, 3), None);
    }

    /// Regression: the pyclasses must stay `Send + Sync` (spec pins frozen,
    /// cross-thread-usable classes; `unsendable` would panic with a
    /// `BaseException`-derived `PanicException` on cross-thread access).
    #[test]
    fn pyclasses_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::SharedDocument>();
        assert_send_sync::<super::Document>();
        assert_send_sync::<super::Page>();
    }
}
