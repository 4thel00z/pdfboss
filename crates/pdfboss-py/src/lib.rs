//! Python bindings for pdfboss, compiled as the extension module
//! `pdfboss._pdfboss` and re-exported by the `pdfboss` package shim.
//!
//! `Document` and `Page` are frozen pyclasses usable from any Python
//! thread. Cheap structural access goes through a lock around one shared
//! parsed document; text extraction and rendering release the GIL and run
//! on a private materialization of the document's shareable core
//! ([`DocumentSeed`]), so heavy calls on different threads run truly in
//! parallel instead of serializing on that lock.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::StreamExt;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyIndexError, PyStopAsyncIteration, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use pyo3::IntoPyObjectExt;

use pdfboss_aio::{AsyncDocument as AioDocument, ElementStream};
use pdfboss_core::elements::{Element as CoreElement, ElementOpts, Elements, XrefKind};
use pdfboss_core::Document as CoreDocument;
use pdfboss_core::Metadata as CoreMetadata;
use pdfboss_core::Page as CorePage;
use pdfboss_core::{Dict, DocumentSeed, ObjRef, Object};
use pdfboss_output::Output;

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

/// Maps a core error to [`PdfError`] with the parse-layer prefix used by
/// the element/async APIs.
fn parse_err(e: pdfboss_core::Error) -> PyErr {
    PdfError::new_err(format!("parse: {e}"))
}

/// Maps an aio error to [`PdfError`], prefixed by the layer it came from
/// ("parse:", "io:" or "http:").
fn aio_err(e: pdfboss_aio::Error) -> PyErr {
    use pdfboss_aio::Error as AioError;
    let msg = match e {
        AioError::Core(e) => format!("parse: {e}"),
        AioError::Io(e) => format!("io: {e}"),
        AioError::Http { status, msg } => match status {
            Some(code) => format!("http: {code}: {msg}"),
            None => format!("http: {msg}"),
        },
        AioError::RangeUnsupported => "http: server does not support Range requests".to_string(),
        AioError::TruncatedRead {
            offset,
            wanted,
            got,
        } => {
            format!("io: truncated read at offset {offset}: wanted {wanted} bytes, got {got}")
        }
    };
    PdfError::new_err(msg)
}

/// Builds the metadata dict; only keys present in the file are included.
fn metadata_dict(py: Python<'_>, meta: CoreMetadata) -> PyResult<Bound<'_, PyDict>> {
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

/// The stable Python `kind` string for a core element variant.
fn kind_str(e: &CoreElement) -> &'static str {
    match e {
        CoreElement::Header { .. } => "header",
        CoreElement::IndirectObject { .. } => "object",
        CoreElement::XrefSection { .. } => "xref",
        CoreElement::Trailer { .. } => "trailer",
        CoreElement::StartXref { .. } => "startxref",
        CoreElement::Eof { .. } => "eof",
        CoreElement::Page { .. } => "page",
        CoreElement::Font { .. } => "font",
        CoreElement::Image { .. } => "image",
        CoreElement::Annotation { .. } => "annotation",
        CoreElement::ContentOp { .. } => "content_op",
    }
}

/// Formats a `(major, minor)` header version as `"major.minor"`.
fn version_string(version: (u8, u8)) -> String {
    format!("{}.{}", version.0, version.1)
}

/// Converts a core [`Object`] to plain Python data: dict/list/str/bytes/
/// int/float/bool/None. Names become `str`; strings decode as UTF-8 where
/// valid, else stay `bytes`; streams become `{"dict": ..., "length": n}`
/// (raw data length in bytes, data not materialized); indirect references
/// become `{"ref": (num, gen)}`.
fn object_to_py<'py>(py: Python<'py>, obj: &Object) -> PyResult<Bound<'py, PyAny>> {
    match obj {
        Object::Null => Ok(py.None().into_bound(py)),
        Object::Bool(b) => (*b).into_bound_py_any(py),
        Object::Int(i) => (*i).into_bound_py_any(py),
        Object::Real(r) => (*r).into_bound_py_any(py),
        Object::String(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => s.into_bound_py_any(py),
            Err(_) => Ok(PyBytes::new(py, bytes).into_any()),
        },
        Object::Name(name) => name.0.as_str().into_bound_py_any(py),
        Object::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(object_to_py(py, item)?)?;
            }
            Ok(list.into_any())
        }
        Object::Dict(dict) => Ok(dict_to_py(py, dict)?.into_any()),
        Object::Stream(stream) => {
            let out = PyDict::new(py);
            out.set_item("dict", dict_to_py(py, &stream.dict)?)?;
            out.set_item("length", stream.data.len())?;
            Ok(out.into_any())
        }
        Object::Ref(r) => {
            let out = PyDict::new(py);
            out.set_item("ref", (r.num, r.gen))?;
            Ok(out.into_any())
        }
    }
}

/// Converts a core [`Dict`] to a Python dict with name-string keys.
fn dict_to_py<'py>(py: Python<'py>, dict: &Dict) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    for (key, value) in dict.iter() {
        out.set_item(key.0.as_str(), object_to_py(py, value)?)?;
    }
    Ok(out)
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

/// Maps the Python `fonts=` string to a [`pdfboss_render::GlyphPainting`] tier.
fn glyph_painting_from_str(s: &str) -> PyResult<pdfboss_render::GlyphPainting> {
    use pdfboss_render::GlyphPainting;
    match s {
        "embedded-only" => Ok(GlyphPainting::EmbeddedTrueTypeOnly),
        "all-embedded" => Ok(GlyphPainting::AllEmbedded),
        "full" => Ok(GlyphPainting::Full),
        other => Err(PyValueError::new_err(format!(
            "unknown fonts mode {other:?}: expected 'embedded-only', 'all-embedded' or 'full'"
        ))),
    }
}

/// The core document behind a lock, shareable across threads.
///
/// [`CoreDocument`] itself is neither `Send` nor `Sync`: its interior
/// object cache uses `RefCell`s holding reference-counted entries. That
/// state is fully encapsulated — no reference-counted pointer or `RefCell`
/// borrow ever escapes the core API (cached objects are handed out as deep
/// clones) — so every touch of it happens inside a method call on the
/// Validates a render request and resolves it to [`RenderOptions`]: the
/// glyph-painting tier from `fonts`, and — at the `full` tier — a substitute
/// face directory from `font_dir` or the optional `pdfboss-fonts` data
/// package. The package import needs the GIL, so this runs on the calling
/// thread, before any `allow_threads`/coroutine boundary; both the sync and
/// the async render route through it, so the two cannot diverge in how they
/// read these arguments.
fn resolve_render_options(
    py: Python<'_>,
    scale: f32,
    fonts: &str,
    font_dir: Option<String>,
) -> PyResult<pdfboss_render::RenderOptions> {
    use pdfboss_render::{GlyphPainting, SubstituteSource};

    if !scale.is_finite() || scale <= 0.0 {
        return Err(PyValueError::new_err(
            "scale must be a positive, finite number",
        ));
    }
    let glyph_painting = glyph_painting_from_str(fonts)?;
    let substitutes = if glyph_painting == GlyphPainting::Full {
        if let Some(dir) = font_dir {
            SubstituteSource::Dir(dir.into())
        } else {
            // The binding discovers the pdfboss-fonts data package.
            match py.import("pdfboss_fonts") {
                Ok(module) => {
                    let dir: String = module.getattr("font_dir")?.call0()?.extract()?;
                    SubstituteSource::Dir(dir.into())
                }
                Err(_) => {
                    return Err(PyValueError::new_err(
                        "fonts=\"full\" requires the pdfboss-fonts package; \
                         install it with `pip install pdfboss[full]`, or pass \
                         an explicit font_dir=...",
                    ));
                }
            }
        }
    } else {
        SubstituteSource::None
    };
    Ok(pdfboss_render::RenderOptions {
        glyph_painting,
        substitutes,
    })
}

/// The one parsed document behind a [`Document`], shared by reference
/// from every handle that needs it. [`CoreDocument`] is single-threaded
/// by design (interior caches), so it never escapes this wrapper except
/// as a [`DocumentSeed`]; all direct access borrows the wrapped value,
/// and the [`Mutex`] serializes those calls.
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
    #[pyo3(signature = (path=None, *, data=None, password=""))]
    fn new(path: Option<PathBuf>, data: Option<Vec<u8>>, password: &str) -> PyResult<Self> {
        let core = match (path, data) {
            (Some(p), None) => CoreDocument::open_with_password(p, password).map_err(pdf_err)?,
            (None, Some(d)) => CoreDocument::load_with_password(d, password).map_err(pdf_err)?,
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
        metadata_dict(py, meta)
    }

    fn __len__(&self) -> usize {
        self.inner.lock().page_count()
    }

    fn __getitem__(&self, index: &Bound<'_, PyAny>) -> PyResult<Page> {
        // Accept an arbitrary Python object so that an index too large to
        // fit in `isize` surfaces as IndexError (the documented behavior)
        // rather than the OverflowError pyo3 would raise while coercing.
        let (seed, page) = {
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
            let page = doc.page(idx).map_err(pdf_err)?;
            // `doc.page` just forced the page tree, so seeding here only
            // clones a few `Arc`s and the key material.
            (doc.seed(), page)
        };
        Ok(Page { seed, page })
    }

    /// Extracts text from all pages, joined by form feed ("\f").
    /// Releases the GIL and fans the pages out across the machine's cores:
    /// each worker thread holds its own fork of the document (shared bytes
    /// and cross-reference table, private caches), pulling page indexes from
    /// a shared counter so a slow page never idles the other cores.
    ///
    /// Per-page lenient like rendering: a page whose content will not
    /// fetch, decode, or parse contributes an empty string rather than
    /// failing the whole document. An error here means the document
    /// itself could not be read.
    fn extract_text(&self, py: Python<'_>) -> PyResult<String> {
        let inner = &self.inner;
        py.allow_threads(move || {
            // The lock is held only long enough to seed; the fan-out runs
            // on a private materialization.
            let doc = CoreDocument::from_seed(inner.lock().seed());
            // `map_pages` visits exactly the materializable pages — the
            // flattened tree, not the declared `/Count`, which on a damaged
            // file can exceed (or fall short of) what the tree yields. One
            // font cache serves every worker, so a font loads once per
            // document rather than once per page.
            let fonts = pdfboss_output::FontCache::default();
            let texts = pdfboss_core::map_pages(&doc, |doc, page| {
                let (text, _) = pdfboss_output::extract_text_reporting_cached(doc, page, &fonts)?;
                Ok(text)
            });
            let mut out = String::new();
            for (i, text) in texts.into_iter().enumerate() {
                if i > 0 {
                    out.push('\u{c}');
                }
                out.push_str(&text.map_err(pdf_err)?);
            }
            Ok(out)
        })
    }

    /// Extracts the whole document as markdown: headings, lists and tables
    /// inferred from layout, with font sizes judged across the document.
    /// Same fan-out and per-page leniency as `extract_text`.
    fn extract_markdown(&self, py: Python<'_>) -> PyResult<String> {
        let inner = &self.inner;
        py.allow_threads(move || {
            let doc = CoreDocument::from_seed(inner.lock().seed());
            pdfboss_output::extract_markdown(&doc).map_err(pdf_err)
        })
    }

    /// Renders every page (or the 0-based `pages` given, in the order given)
    /// to PNG bytes, fanned out across the machine's cores — same arguments
    /// and leniency as `Page.render`, one PNG per page. For a multi-page
    /// document this is the convenient fast path: one call renders them all
    /// at once, where per-page `render` calls only parallelize if you run
    /// them from your own threads.
    #[pyo3(signature = (pages=None, scale=1.0, fonts="all-embedded", font_dir=None))]
    fn render_pages<'py>(
        &self,
        py: Python<'py>,
        pages: Option<Vec<usize>>,
        scale: f32,
        fonts: &str,
        font_dir: Option<String>,
    ) -> PyResult<Vec<Bound<'py, PyBytes>>> {
        let opts = resolve_render_options(py, scale, fonts, font_dir)?;
        let inner = &self.inner;
        let pngs = py.allow_threads(move || {
            // The lock is held only long enough to seed; the fan-out runs
            // on a private materialization.
            let doc = CoreDocument::from_seed(inner.lock().seed());
            let outcomes = match &pages {
                None => pdfboss_core::map_pages(&doc, |doc, page| {
                    let (pix, _) = pdfboss_render::render_page_reporting(doc, page, scale, &opts)?;
                    pix.encode_png()
                }),
                Some(wanted) => {
                    // An explicit list renders exactly those pages, in the
                    // order given. The fan-out still runs over the whole
                    // selection: each worker resolves its own page index.
                    let selection: Vec<usize> = wanted.clone();
                    let seed = doc.seed();
                    let next = std::sync::atomic::AtomicUsize::new(0);
                    let slots: Vec<std::sync::OnceLock<Result<Vec<u8>, pdfboss_core::Error>>> = (0
                        ..selection.len())
                        .map(|_| std::sync::OnceLock::new())
                        .collect();
                    let workers = std::thread::available_parallelism()
                        .map(std::num::NonZeroUsize::get)
                        .unwrap_or(1)
                        .min(selection.len().max(1));
                    std::thread::scope(|scope| {
                        for _ in 0..workers {
                            let seed = seed.clone();
                            let (next, slots, selection, opts) = (&next, &slots, &selection, &opts);
                            scope.spawn(move || {
                                let worker = pdfboss_core::Document::from_seed(seed);
                                loop {
                                    let s = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    if s >= selection.len() {
                                        break;
                                    }
                                    let outcome = worker.page(selection[s]).and_then(|page| {
                                        let (pix, _) = pdfboss_render::render_page_reporting(
                                            &worker, &page, scale, opts,
                                        )?;
                                        pix.encode_png()
                                    });
                                    slots[s].set(outcome).ok();
                                }
                            });
                        }
                    });
                    slots
                        .into_iter()
                        .map(|slot| slot.into_inner().expect("every slot was dispatched"))
                        .collect()
                }
            };
            outcomes
                .into_iter()
                .map(|png| png.map_err(pdf_err))
                .collect::<PyResult<Vec<Vec<u8>>>>()
        })?;
        Ok(pngs.into_iter().map(|png| PyBytes::new(py, &png)).collect())
    }

    /// Lazily iterates the document's elements: physical file structure in
    /// file order, then logical document structure in document order.
    /// Nothing is parsed or decoded before it is yielded.
    #[pyo3(signature = (*, physical=true, logical=true, pages=None, content_ops=false))]
    fn elements(
        &self,
        physical: bool,
        logical: bool,
        pages: Option<Vec<usize>>,
        content_ops: bool,
    ) -> ElementIter {
        let opts = ElementOpts {
            physical,
            logical,
            pages,
            content_ops,
        };
        let doc = Arc::clone(&self.inner);
        let iter = {
            let guard = doc.lock();
            let core: &CoreDocument = &guard;
            // SAFETY: the borrow is extended to 'static. The Arc stored in
            // the returned ElementIter keeps the CoreDocument alive at a
            // stable heap address (it lives inside the Arc'd
            // SharedDocument), and ElementIter only advances the iterator
            // while re-holding the document mutex. See SharedElements.
            let core: &'static CoreDocument =
                unsafe { std::mem::transmute::<&CoreDocument, &'static CoreDocument>(core) };
            SharedElements(Mutex::new(core.elements(opts)))
        };
        ElementIter { doc, iter }
    }
}

/// A single page of a document.
#[pyclass(frozen)]
struct Page {
    /// The document's shareable core. Every text or render call
    /// materializes its own private document from this, so per-page work
    /// holds no lock and runs concurrently across Python threads.
    seed: DocumentSeed,
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

    /// Extracts the page's text. Releases the GIL and runs on a private
    /// materialization of the document, so extractions of different pages
    /// proceed in parallel when called from multiple Python threads.
    fn extract_text(&self, py: Python<'_>) -> PyResult<String> {
        py.allow_threads(|| {
            let doc = CoreDocument::from_seed(self.seed.clone());
            pdfboss_output::extract_text(&doc, &self.page).map_err(pdf_err)
        })
    }

    /// Extracts the page's markdown, ranking heading sizes against that page
    /// alone. `Document.extract_markdown` is the better answer whenever the
    /// whole document is at hand.
    fn extract_markdown(&self, py: Python<'_>) -> PyResult<String> {
        py.allow_threads(|| {
            let doc = CoreDocument::from_seed(self.seed.clone());
            pdfboss_output::extract_page_markdown(&doc, &self.page).map_err(pdf_err)
        })
    }

    /// Renders the page and returns PNG bytes. Releases the GIL while the
    /// rasterization and PNG encoding run, on a private materialization of
    /// the document — no lock is held, so renders called from multiple
    /// Python threads run truly in parallel (or let
    /// `Document.render_pages` do the fan-out for you).
    ///
    /// `fonts="full"` substitutes replacement faces for non-embedded fonts.
    /// Faces come from an explicit `font_dir=...`, or else are discovered by
    /// importing the optional `pdfboss-fonts` data package; if neither is
    /// available this raises `ValueError` with an actionable install
    /// message rather than silently degrading or leaking a raw import
    /// error.
    ///
    /// Content pdfboss cannot read is skipped, so a page can come out blank
    /// without raising. Use `render_reporting` to see what was dropped.
    #[pyo3(signature = (scale=1.0, fonts="all-embedded", font_dir=None))]
    fn render<'py>(
        &self,
        py: Python<'py>,
        scale: f32,
        fonts: &str,
        font_dir: Option<String>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let rendered = self.render_reporting(py, scale, fonts, font_dir)?;
        Ok(rendered.0)
    }

    /// Renders the page like `render`, returning `(png_bytes, warnings)`.
    ///
    /// `warnings` is one human-readable line per distinct piece of content
    /// the render had to drop or approximate, e.g. `"1 image skipped:
    /// unsupported filter /Crypt"`. It is empty when the page
    /// rasterized exactly as it describes itself, so a blank page is never
    /// mistaken for a clean render.
    #[pyo3(signature = (scale=1.0, fonts="all-embedded", font_dir=None))]
    fn render_reporting<'py>(
        &self,
        py: Python<'py>,
        scale: f32,
        fonts: &str,
        font_dir: Option<String>,
    ) -> PyResult<(Bound<'py, PyBytes>, Vec<String>)> {
        let opts = resolve_render_options(py, scale, fonts, font_dir)?;
        let (png, warnings) = py.allow_threads(|| {
            let doc = CoreDocument::from_seed(self.seed.clone());
            let (pixmap, report) =
                pdfboss_render::render_page_reporting(&doc, &self.page, scale, &opts)
                    .map_err(pdf_err)?;
            Ok::<_, PyErr>((pixmap.encode_png().map_err(pdf_err)?, report.warnings()))
        })?;
        Ok((PyBytes::new(py, &png), warnings))
    }
}

/// One element of a PDF: physical file structure (header, indirect
/// objects, xref sections, trailer, startxref, eof — always with byte
/// spans) or logical document structure (pages, fonts, images,
/// annotations, content ops).
#[pyclass(frozen)]
struct Element {
    inner: CoreElement,
}

#[pymethods]
impl Element {
    /// The element kind: "header", "object", "xref", "trailer",
    /// "startxref", "eof", "page", "font", "image", "annotation" or
    /// "content_op".
    #[getter]
    fn kind(&self) -> &'static str {
        kind_str(&self.inner)
    }

    /// Byte range as `(start, end)`, end-exclusive. Physical elements:
    /// the range in the file. Content ops: the range within the page's
    /// decoded, concatenated content stream. Other logical elements: None.
    #[getter]
    fn span(&self) -> Option<(u64, u64)> {
        match &self.inner {
            CoreElement::Header { span, .. }
            | CoreElement::IndirectObject { span, .. }
            | CoreElement::XrefSection { span, .. }
            | CoreElement::Trailer { span, .. }
            | CoreElement::StartXref { span, .. }
            | CoreElement::Eof { span } => Some((span.start, span.end)),
            CoreElement::ContentOp {
                span_in_content, ..
            } => Some((span_in_content.start, span_in_content.end)),
            CoreElement::Page { .. }
            | CoreElement::Font { .. }
            | CoreElement::Image { .. }
            | CoreElement::Annotation { .. } => None,
        }
    }

    /// The `(num, gen)` object reference, where applicable.
    #[getter]
    fn r#ref(&self) -> Option<(u32, u16)> {
        match &self.inner {
            CoreElement::IndirectObject { r, .. }
            | CoreElement::Page { r, .. }
            | CoreElement::Font { r, .. }
            | CoreElement::Image { r, .. }
            | CoreElement::Annotation { r, .. } => Some((r.num, r.gen)),
            CoreElement::Header { .. }
            | CoreElement::XrefSection { .. }
            | CoreElement::Trailer { .. }
            | CoreElement::StartXref { .. }
            | CoreElement::Eof { .. }
            | CoreElement::ContentOp { .. } => None,
        }
    }

    /// The 0-based page index for logical elements, None otherwise.
    #[getter]
    fn page(&self) -> Option<usize> {
        match &self.inner {
            CoreElement::Page { index, .. } => Some(*index),
            CoreElement::Font { page, .. } | CoreElement::Image { page, .. } => *page,
            CoreElement::Annotation { page, .. } | CoreElement::ContentOp { page, .. } => {
                Some(*page)
            }
            CoreElement::Header { .. }
            | CoreElement::IndirectObject { .. }
            | CoreElement::XrefSection { .. }
            | CoreElement::Trailer { .. }
            | CoreElement::StartXref { .. }
            | CoreElement::Eof { .. } => None,
        }
    }

    /// Lazily converts the element's payload to plain Python data:
    /// dict/list/str/bytes/int/float/bool/None. Objects and the trailer
    /// convert fully (names -> str, strings -> str where UTF-8-valid else
    /// bytes, streams -> {"dict": ..., "length": int}, references ->
    /// {"ref": (num, gen)}). Header -> the version string; xref ->
    /// {"kind": ..., "entries": ...}; startxref -> int; font ->
    /// {"subtype": ..., "base_font": ...}; image -> {"width": ...,
    /// "height": ...}; annotation -> {"subtype": ...}; content ops -> the
    /// operator rendered as a string; eof and page -> None.
    fn value<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match &self.inner {
            CoreElement::Header { version, .. } => version_string(*version).into_bound_py_any(py),
            CoreElement::IndirectObject { object, .. } => object_to_py(py, object),
            CoreElement::XrefSection { kind, entries, .. } => {
                let out = PyDict::new(py);
                out.set_item(
                    "kind",
                    match kind {
                        XrefKind::Table => "table",
                        XrefKind::Stream => "stream",
                    },
                )?;
                out.set_item("entries", *entries)?;
                Ok(out.into_any())
            }
            CoreElement::Trailer { dict, .. } => Ok(dict_to_py(py, dict)?.into_any()),
            CoreElement::StartXref { offset, .. } => (*offset).into_bound_py_any(py),
            CoreElement::Eof { .. } | CoreElement::Page { .. } => Ok(py.None().into_bound(py)),
            CoreElement::Font {
                subtype, base_font, ..
            } => {
                let out = PyDict::new(py);
                out.set_item("subtype", subtype.0.as_str())?;
                out.set_item("base_font", base_font.as_ref().map(|n| n.0.as_str()))?;
                Ok(out.into_any())
            }
            CoreElement::Image { width, height, .. } => {
                let out = PyDict::new(py);
                out.set_item("width", *width)?;
                out.set_item("height", *height)?;
                Ok(out.into_any())
            }
            CoreElement::Annotation { subtype, .. } => {
                let out = PyDict::new(py);
                out.set_item("subtype", subtype.0.as_str())?;
                Ok(out.into_any())
            }
            CoreElement::ContentOp { op, .. } => format!("{op:?}").into_bound_py_any(py),
        }
    }
}

/// The core element iterator with its document borrow extended to
/// `'static`, lockable for exclusive advancement.
///
/// Safety invariants (upheld by `Document::elements` and `ElementIter`):
///
/// - the `Arc<SharedDocument>` stored next to this in `ElementIter` keeps
///   the borrowed `CoreDocument` alive (at a stable heap address inside
///   the Arc) for the iterator's whole lifetime, and
/// - the iterator is only ever advanced while the document mutex is held,
///   which serializes every touch of the document's interior caches.
struct SharedElements(Mutex<Elements<'static>>);

// SAFETY: `Elements<'static>` embeds a `&CoreDocument`, which is neither
// `Send` nor `Sync` because of the document's interior object cache. Per
// the invariants above, that borrow is only dereferenced under the same
// mutex that makes `SharedDocument` sound, so moving or sharing this
// wrapper across threads cannot race.
unsafe impl Send for SharedElements {}
unsafe impl Sync for SharedElements {}

impl SharedElements {
    /// Locks the iterator state. A poisoned lock is recovered, matching
    /// `SharedDocument::lock`.
    fn lock(&self) -> MutexGuard<'_, Elements<'static>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Sync iterator over a document's elements, returned by
/// `Document.elements()`.
#[pyclass(frozen)]
struct ElementIter {
    // Declared before `doc`: fields drop in declaration order, and `iter`
    // borrows the document (via the 'static-extended `Elements` inside
    // `SharedElements`), so it must drop before the `Arc` that keeps that
    // borrowed document alive.
    iter: SharedElements,
    doc: Arc<SharedDocument>,
}

#[pymethods]
impl ElementIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Advances the underlying core iterator. Releases the GIL while the
    /// next element is located and parsed. Per-item parse failures raise
    /// PdfError for that item; iteration may be continued afterwards
    /// (salvage semantics).
    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Element>> {
        let item = py.allow_threads(|| {
            let doc = self.doc.lock();
            let next = self.iter.lock().next();
            drop(doc);
            next
        });
        match item {
            None => Ok(None),
            Some(Ok(element)) => Ok(Some(Element { inner: element })),
            Some(Err(e)) => Err(parse_err(e)),
        }
    }
}

/// A PDF document opened for async I/O. Constructors and data-fetching
/// methods are coroutines driven by one global multi-thread tokio
/// runtime; `page_count`/`version` are sync because the open flow already
/// parsed the xref chain and page tree index.
#[pyclass(frozen)]
struct AsyncDocument {
    inner: AioDocument,
}

#[pymethods]
impl AsyncDocument {
    /// Opens a PDF file for async access. Coroutine resolving to an
    /// AsyncDocument. The whole file is never read eagerly. `password`
    /// opens an encrypted file, as the user or the owner password.
    #[staticmethod]
    #[pyo3(signature = (path, *, password=String::new()))]
    fn open(py: Python<'_>, path: PathBuf, password: String) -> PyResult<Bound<'_, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let inner = AioDocument::open_with_password(path, &password)
                .await
                .map_err(aio_err)?;
            Ok(AsyncDocument { inner })
        })
    }

    /// Loads a PDF from bytes already in memory. Coroutine resolving to
    /// an AsyncDocument. `password` opens an encrypted file, as the user
    /// or the owner password.
    #[staticmethod]
    #[pyo3(signature = (data, *, password=String::new()))]
    fn from_bytes(py: Python<'_>, data: Vec<u8>, password: String) -> PyResult<Bound<'_, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let inner = AioDocument::from_bytes_with_password(data, &password)
                .await
                .map_err(aio_err)?;
            Ok(AsyncDocument { inner })
        })
    }

    /// Opens a PDF over HTTP using range requests; the whole file is
    /// never downloaded. The server must honor `Range` (a server that
    /// ignores it raises PdfError with an "http:" message). Coroutine
    /// resolving to an AsyncDocument. `password` opens an encrypted file,
    /// as the user or the owner password.
    #[staticmethod]
    #[pyo3(signature = (url, *, password=String::new()))]
    fn open_url(py: Python<'_>, url: String, password: String) -> PyResult<Bound<'_, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let inner = AioDocument::open_url_with_password(url, &password)
                .await
                .map_err(aio_err)?;
            Ok(AsyncDocument { inner })
        })
    }

    /// Number of pages in the document. A property, exactly like the sync
    /// `Document.page_count`: the open flow already parsed the page tree, so
    /// nothing here awaits.
    #[getter]
    fn page_count(&self) -> usize {
        self.inner.page_count()
    }

    /// PDF version from the file header, e.g. "1.7". A property, like the
    /// sync `Document.version`.
    #[getter]
    fn version(&self) -> String {
        version_string(self.inner.version())
    }

    fn __len__(&self) -> usize {
        self.inner.page_count()
    }

    fn __getitem__(&self, index: &Bound<'_, PyAny>) -> PyResult<AsyncPage> {
        // Mirrors the sync `Document.__getitem__`: negative indexes count
        // from the end, and anything unrepresentable is IndexError.
        let count = self.inner.page_count();
        let idx = index
            .extract::<isize>()
            .ok()
            .and_then(|i| normalize_index(i, count));
        let Some(idx) = idx else {
            return Err(PyIndexError::new_err(format!(
                "page index {index} out of range ({count} pages)"
            )));
        };
        let page = self.inner.page(idx).map_err(aio_err)?;
        Ok(AsyncPage {
            doc: self.inner.clone(),
            page,
        })
    }

    /// The page at 0-based `index`. Synchronous — the page tree and its
    /// inherited attributes were resolved at open — and negative indexes are
    /// NOT accepted here (use subscription, `doc[-1]`, for that), mirroring
    /// the sync `Document.page` split.
    fn page(&self, index: usize) -> PyResult<AsyncPage> {
        let page = self.inner.page(index).map_err(aio_err)?;
        Ok(AsyncPage {
            doc: self.inner.clone(),
            page,
        })
    }

    /// Extracts text from all pages, joined by form feed ("\f"), like the
    /// sync `Document.extract_text` — including its per-page leniency: a
    /// page whose content will not read contributes an empty string, and
    /// an error means the document itself could not be read. Coroutine;
    /// the extraction runs on the tokio runtime, so the asyncio loop is
    /// never blocked.
    fn extract_text<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut out = String::new();
            for i in 0..inner.page_count() {
                if i > 0 {
                    out.push('\u{c}');
                }
                let page = inner.page(i).map_err(aio_err)?;
                let text = pdfboss_output::extract_text_with(inner.clone(), &page)
                    .await
                    .map_err(pdf_err)?;
                out.push_str(&text);
            }
            Ok(out)
        })
    }

    /// Extracts the whole document as markdown, like the sync
    /// `Document.extract_markdown` — headings, lists and tables inferred
    /// from layout, font sizes judged across the document. Coroutine; runs
    /// on the tokio runtime, so the asyncio loop is never blocked.
    fn extract_markdown<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut pages = Vec::new();
            for i in 0..inner.page_count() {
                let page = inner.page(i).map_err(aio_err)?;
                let (spans, rulings, _) =
                    pdfboss_text::extract_spans_and_rulings_reporting_with(inner.clone(), &page)
                        .await
                        .map_err(pdf_err)?;
                pages.push((spans, rulings));
            }
            Ok(pdfboss_output::Markdown
                .render(&pdfboss_output::document_layout_with_rulings(&pages)))
        })
    }

    /// Renders every page (or the 0-based `pages` given, in the order given)
    /// to PNG bytes — the async twin of `Document.render_pages`, same
    /// arguments, same leniency, one PNG per page. Coroutine resolving to a
    /// list of bytes.
    ///
    /// The fan-out shape matches the sync one: one worker per core, each
    /// pulling the next page index from a shared counter so a slow page
    /// never idles the others — except the workers are tokio tasks, so the
    /// asyncio loop stays free and it works over any source, including
    /// `open_url` documents (each worker range-fetches what its page needs).
    #[pyo3(signature = (pages=None, scale=1.0, fonts="all-embedded", font_dir=None))]
    fn render_pages<'py>(
        &self,
        py: Python<'py>,
        pages: Option<Vec<usize>>,
        scale: f32,
        fonts: &str,
        font_dir: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = resolve_render_options(py, scale, fonts, font_dir)?;
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let selection: Arc<Vec<usize>> = Arc::new(match pages {
                Some(wanted) => wanted,
                None => (0..inner.page_count()).collect(),
            });
            let opts = Arc::new(opts);
            let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let workers = std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1)
                .min(selection.len().max(1));
            let mut handles = Vec::with_capacity(workers);
            for _ in 0..workers {
                let (inner, opts, next, selection) = (
                    inner.clone(),
                    Arc::clone(&opts),
                    Arc::clone(&next),
                    Arc::clone(&selection),
                );
                handles.push(tokio::spawn(async move {
                    let mut done: Vec<(usize, PyResult<Vec<u8>>)> = Vec::new();
                    loop {
                        let s = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if s >= selection.len() {
                            break;
                        }
                        let outcome = match inner.page(selection[s]).map_err(aio_err) {
                            Err(e) => Err(e),
                            Ok(page) => pdfboss_render::render_page_reporting_with(
                                inner.clone(),
                                &page,
                                scale,
                                &opts,
                            )
                            .await
                            .and_then(|(pix, _)| pix.encode_png())
                            .map_err(pdf_err),
                        };
                        done.push((s, outcome));
                    }
                    done
                }));
            }
            let mut pngs: Vec<Option<Vec<u8>>> = vec![None; selection.len()];
            for handle in handles {
                let done = handle
                    .await
                    .map_err(|e| PdfError::new_err(format!("render worker failed: {e}")))?;
                for (s, outcome) in done {
                    pngs[s] = Some(outcome?);
                }
            }
            Python::with_gil(|py| {
                let list: Vec<Py<PyAny>> = pngs
                    .into_iter()
                    .map(|png| {
                        PyBytes::new(py, &png.expect("every slot was dispatched"))
                            .into_any()
                            .unbind()
                    })
                    .collect();
                Ok::<Vec<Py<PyAny>>, PyErr>(list)
            })
        })
    }

    /// Document metadata; only keys present in the file are included.
    /// Coroutine resolving to a dict.
    fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let meta = inner.metadata().await.map_err(aio_err)?;
            Python::with_gil(|py| {
                Ok::<Py<PyAny>, PyErr>(metadata_dict(py, meta)?.into_any().unbind())
            })
        })
    }

    /// Fetches and parses the indirect object `num gen`, returning its
    /// converted Python value. Coroutine.
    #[pyo3(signature = (num, gen=0))]
    fn get_object<'py>(&self, py: Python<'py>, num: u32, gen: u16) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let object = inner
                .get_object(ObjRef { num, gen })
                .await
                .map_err(aio_err)?;
            Python::with_gil(|py| object_to_py(py, &object).map(Bound::unbind))
        })
    }

    /// Streams the document's elements; use with `async for`. Same
    /// ordering and salvage semantics as `Document.elements`.
    #[pyo3(signature = (*, physical=true, logical=true, pages=None, content_ops=false))]
    fn elements(
        &self,
        physical: bool,
        logical: bool,
        pages: Option<Vec<usize>>,
        content_ops: bool,
    ) -> AsyncElementIter {
        let opts = ElementOpts {
            physical,
            logical,
            pages,
            content_ops,
        };
        AsyncElementIter {
            stream: Arc::new(tokio::sync::Mutex::new(self.inner.elements(opts))),
        }
    }
}

/// A single page of an async document. Attributes are synchronous — the
/// page tree and its inherited attributes were resolved at open — while
/// `extract_text` and the render methods are coroutines, driving the SAME
/// shared implementations the sync `Page` drives, over the async document's
/// range-fetching reads.
#[pyclass(frozen)]
struct AsyncPage {
    doc: AioDocument,
    page: CorePage,
}

#[pymethods]
impl AsyncPage {
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

    /// Extracts the page's text. Coroutine resolving to str.
    fn extract_text<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let doc = self.doc.clone();
        let page = self.page.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            pdfboss_output::extract_text_with(doc, &page)
                .await
                .map_err(pdf_err)
        })
    }

    /// Extracts the page's markdown, like the sync `Page.extract_markdown`,
    /// ranking heading sizes against that page alone. Coroutine.
    fn extract_markdown<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let doc = self.doc.clone();
        let page = self.page.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            pdfboss_output::extract_page_markdown_with(doc, &page)
                .await
                .map_err(pdf_err)
        })
    }

    /// Renders the page and resolves to PNG bytes; same arguments and
    /// leniency as the sync `Page.render`. Coroutine.
    #[pyo3(signature = (scale=1.0, fonts="all-embedded", font_dir=None))]
    fn render<'py>(
        &self,
        py: Python<'py>,
        scale: f32,
        fonts: &str,
        font_dir: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = resolve_render_options(py, scale, fonts, font_dir)?;
        let doc = self.doc.clone();
        let page = self.page.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (pixmap, _) = pdfboss_render::render_page_reporting_with(doc, &page, scale, &opts)
                .await
                .map_err(pdf_err)?;
            let png = pixmap.encode_png().map_err(pdf_err)?;
            Python::with_gil(|py| {
                Ok::<Py<PyAny>, PyErr>(PyBytes::new(py, &png).into_any().unbind())
            })
        })
    }

    /// Renders the page like `render`, resolving to `(png_bytes, warnings)`;
    /// same reporting semantics as the sync `Page.render_reporting`.
    /// Coroutine.
    #[pyo3(signature = (scale=1.0, fonts="all-embedded", font_dir=None))]
    fn render_reporting<'py>(
        &self,
        py: Python<'py>,
        scale: f32,
        fonts: &str,
        font_dir: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = resolve_render_options(py, scale, fonts, font_dir)?;
        let doc = self.doc.clone();
        let page = self.page.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (pixmap, report) =
                pdfboss_render::render_page_reporting_with(doc, &page, scale, &opts)
                    .await
                    .map_err(pdf_err)?;
            let png = pixmap.encode_png().map_err(pdf_err)?;
            let warnings = report.warnings();
            Python::with_gil(|py| {
                let bytes = PyBytes::new(py, &png).into_any().unbind();
                Ok::<(Py<PyAny>, Vec<String>), PyErr>((bytes, warnings))
            })
        })
    }
}

/// Async iterator over a document's elements, returned by
/// `AsyncDocument.elements()`. Each `__anext__` is a coroutine driving
/// the Rust element stream on the tokio runtime, so the asyncio loop is
/// never blocked.
///
/// `ElementStream` is owned and `'static` (it holds a cheap `Arc` clone of
/// the document, not a borrow of it — see `pdfboss_aio::stream`), so unlike
/// the sync `ElementIter` this needs no borrow-extension or drop-order
/// trick: the `Arc<tokio::sync::Mutex<_>>` alone keeps it alive and
/// serializes advancement across concurrent `__anext__` calls.
#[pyclass(frozen)]
struct AsyncElementIter {
    stream: Arc<tokio::sync::Mutex<ElementStream>>,
}

#[pymethods]
impl AsyncElementIter {
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Coroutine resolving to the next Element; raises StopAsyncIteration
    /// when the stream is exhausted. Per-item failures raise PdfError for
    /// that item and the stream may be continued (salvage semantics).
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = Arc::clone(&self.stream);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut stream = stream.lock().await;
            match stream.next().await {
                Some(Ok(element)) => Ok(Element { inner: element }),
                Some(Err(e)) => Err(aio_err(e)),
                None => Err(PyStopAsyncIteration::new_err("element stream exhausted")),
            }
        })
    }
}

#[pymodule]
fn _pdfboss(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("PdfError", m.py().get_type::<PdfError>())?;
    m.add_class::<Document>()?;
    m.add_class::<AsyncPage>()?;
    m.add_class::<Page>()?;
    m.add_class::<Element>()?;
    m.add_class::<ElementIter>()?;
    m.add_class::<AsyncDocument>()?;
    m.add_class::<AsyncElementIter>()?;
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
        assert_send_sync::<super::Element>();
        assert_send_sync::<super::ElementIter>();
        assert_send_sync::<super::AsyncDocument>();
        assert_send_sync::<super::AsyncElementIter>();
    }

    #[test]
    fn kind_str_maps_variants_to_kind_names() {
        use pdfboss_core::elements::{Element as CoreElement, Span};
        let span = Span { start: 0, end: 9 };
        assert_eq!(
            super::kind_str(&CoreElement::Header {
                version: (1, 7),
                span
            }),
            "header"
        );
        assert_eq!(super::kind_str(&CoreElement::Eof { span }), "eof");
    }
}
