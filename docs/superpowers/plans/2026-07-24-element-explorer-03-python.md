# pdfboss Python Bindings (elements + async) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the element iterator and the async document API to Python: sync `Document.elements()` yielding `Element` objects, and a new `AsyncDocument` usable with `await` / `async for` from asyncio.

**Architecture:** All binding code lives in `crates/pdfboss-py/src/lib.rs`, following its existing patterns (frozen pyclasses, `Arc<SharedDocument>` sharing, everything mapped to `PdfError`). Sync iteration wraps `pdfboss-core`'s `Elements<'_>` iterator (plan 01) behind the existing document mutex, releasing the GIL on every `__next__`. Async support wraps `pdfboss-aio` (plan 02) through `pyo3-async-runtimes` on its one global multi-thread tokio runtime; `AsyncDocument.elements()` returns an `__aiter__`/`__anext__` object where each `__anext__` is a coroutine driving the Rust `ElementStream`, so the asyncio loop is never blocked.

**Tech Stack:**
- Rust: `pyo3 0.25` (abi3-py312), `pyo3-async-runtimes 0.25` (tokio-runtime), `tokio` (sync), `futures-util`, `pdfboss-core` (elements), `pdfboss-aio` (with `http` feature).
- Python: `maturin` build backend, `pytest` + `pytest-asyncio`, stdlib `http.server` for the in-test Range server.
- Tooling: `uv` for the local env (`uv sync`, `uv run --no-sync pytest`, `uv run --no-sync maturin develop --uv`); CI stays on `pip install .` + `pytest`.

## Global Constraints

- **Cleanroom rule (from the 2026-07-12 spec, unchanged):** everything is implemented purely from ISO 32000; NEVER name any other PDF library anywhere — code, comments, docs, tests, commits, plan prose. Non-PDF dependencies (tokio, futures, pyo3, reqwest) are fine.
- **`pdfboss-core` gains zero new dependencies.** This plan touches only `crates/pdfboss-py`, `python/`, `tests/`, `pyproject.toml`, and `.github/workflows/python-ci.yml`.
- **The existing sync API is untouched:** `Document`, `Page`, their constructors, properties, `extract_text`, `render`, and all existing tests keep working unchanged. New capability is additive. (Internal refactors that preserve behavior — e.g. extracting a `metadata_dict` helper — are allowed.)
- **NEVER create underscore-prefixed identifiers for NEW names** — no `_foo` methods, attributes, or variables (including `_` loop variables) anywhere in new code. The existing `_pdfboss` extension-module name stays as-is.
- Edition 2021 (workspace `edition.workspace = true`).
- clippy/fmt clean after every task: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` must pass.
- **All builds use the shared cargo target dir:** export `CARGO_TARGET_DIR=$HOME/.cargo/shared-target` for every `cargo`/`maturin`/`uv sync` invocation. Never create per-agent target dirs; never run `cargo clean`.
- **`maturin develop` for local iteration:** after every Rust change, rebuild the extension with `CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv run --no-sync maturin develop --uv` before running pytest. Use `uv run --no-sync pytest …` afterwards (plain `uv run` would re-sync and reinstall the project wheel, clobbering the develop build).
- **Prerequisites:** plans 01 (`pdfboss-core::elements`) and 02 (`pdfboss-aio`) are merged. Verify before starting: `grep -n "pub fn elements" crates/pdfboss-core/src/elements.rs` and `ls crates/pdfboss-aio/src`. If either is missing, STOP — this plan consumes their APIs as a contract.
- **aio path contract:** plan 02 re-exports `AsyncDocument`, `ElementStream`, `Error`, and `Result` at the `pdfboss_aio` crate root (mirroring `pdfboss-core`'s root re-export convention). If plan 02 placed them only in submodules, adjust the `use` lines (`pdfboss_aio::document::AsyncDocument`, `pdfboss_aio::stream::ElementStream`, `pdfboss_aio::error::{Error, Result}`) — nothing else changes.
- Conventional-commit messages; one commit per task.
- Rust gates to run for every task that touches Rust (with `CARGO_TARGET_DIR=$HOME/.cargo/shared-target` exported):
  ```bash
  cargo test -p pdfboss-py
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --all -- --check
  ```

### Design decisions pinned by this plan (resolving spec silences)

- `Element.span` returns the physical byte span for physical elements; for `content_op` it returns `span_in_content` (the byte range within the page's decoded, concatenated content stream — documented in the stub); `None` for `page`/`font`/`image`/`annotation`.
- `Element.value()` per kind: `header` → the version `str` (e.g. `"1.7"`); `object` → the converted object; `xref` → `{"kind": "table"|"stream", "entries": int}`; `trailer` → the converted trailer dict; `startxref` → `int` offset; `eof` → `None`; `page` → `None`; `font` → `{"subtype": str, "base_font": str | None}`; `image` → `{"width": int, "height": int}`; `annotation` → `{"subtype": str}`; `content_op` → the operator's debug rendering as `str`.
- Indirect references inside converted objects become `{"ref": (num, gen)}` (a one-key dict, so it can never be confused with a PDF array).
- Layer-prefixed error messages (`"parse: …"`, `"io: …"`, `"http: …"`) apply to the NEW APIs (elements + async). Existing `Document`/`Page` error messages stay byte-identical (existing sync API untouched).

### Task 1: Sync `Document.elements()` with the `Element` and `ElementIter` pyclasses

**Files:**
- Modify: `crates/pdfboss-py/src/lib.rs` (imports at lines 10–19; new items inserted after the `Page` `#[pymethods]` block ending at line 306; new method appended inside `#[pymethods] impl Document` before its closing brace at line 203; `#[pymodule]` at lines 308–315; unit tests at lines 317–368)
- Modify: `python/pdfboss/__init__.py` (whole file, 5 lines)
- Test: `tests/test_elements.py` (new)

**Interfaces:**

Consumes (exact contract from plan 01 via the spec, `pdfboss-core::elements`):

```rust
/// Byte range in the physical file, end-exclusive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span { pub start: u64, pub end: u64 }

#[derive(Clone, Debug)]
pub enum Element {
    Header { version: (u8, u8), span: Span },
    IndirectObject { r: ObjRef, object: Object, span: Span, in_objstm: Option<(ObjRef, Span)> },
    XrefSection { kind: XrefKind, span: Span, entries: usize },
    Trailer { dict: Dict, span: Span },
    StartXref { offset: u64, span: Span },
    Eof { span: Span },
    Page { index: usize, r: ObjRef },
    Font { page: Option<usize>, r: ObjRef, subtype: Name, base_font: Option<Name> },
    Image { page: Option<usize>, r: ObjRef, width: u32, height: u32 },
    Annotation { page: usize, r: ObjRef, subtype: Name },
    ContentOp { page: usize, op: content::Op, span_in_content: Span },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XrefKind { Table, Stream }

#[derive(Clone, Debug, Default)]
pub struct ElementOpts {
    pub physical: bool,            // default true
    pub logical: bool,             // default true
    pub pages: Option<Vec<usize>>, // logical layer: restrict to these pages
    pub content_ops: bool,         // default false — ops are high-volume
}

impl Document {
    pub fn elements(&self, opts: ElementOpts) -> Elements<'_>;
}

pub struct Elements<'a> { /* iterator state machine */ }
impl<'a> Iterator for Elements<'a> {
    type Item = Result<Element>;
}
```

Also consumes from existing code actually read: `pdfboss_core::Document` (`crates/pdfboss-core/src/lib.rs` re-export), `ObjRef { num: u32, gen: u16 }` and `Name(pub String)` (`crates/pdfboss-core/src/object.rs`), and the existing binding items `SharedDocument`, `SharedDocument::lock`, `pdf_err`, `PdfError`, `Document { inner: Arc<SharedDocument> }` (`crates/pdfboss-py/src/lib.rs`).

Produces (Python surface later tasks rely on):

```python
class Element:
    kind: str                      # "header" | "object" | "xref" | "trailer" |
                                   # "startxref" | "eof" | "page" | "font" |
                                   # "image" | "annotation" | "content_op"
    span: tuple[int, int] | None
    ref: tuple[int, int] | None
    page: int | None

class ElementIter:
    def __iter__(self) -> "ElementIter": ...
    def __next__(self) -> Element: ...

class Document:
    def elements(self, *, physical: bool = True, logical: bool = True,
                 pages: list[int] | None = None,
                 content_ops: bool = False) -> Iterator[Element]: ...
```

Also produces (Rust, consumed by Tasks 2–4): `struct Element { inner: CoreElement }`, `fn kind_str(e: &CoreElement) -> &'static str`, `fn parse_err(e: pdfboss_core::Error) -> PyErr`.

- [ ] **Step 1: Set up the Python environment** (skip pieces that already exist)
  ```bash
  cd /Users/mohamed.tahrioui/private/pdfboss
  grep -n "pub fn elements" crates/pdfboss-core/src/elements.rs   # prerequisite: plan 01 merged
  ls crates/pdfboss-aio/src                                       # prerequisite: plan 02 merged
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv sync
  ```
  `uv sync` builds the extension once via the maturin backend and installs the `dev` dependency group (`maturin`, `pytest`, `pyyaml`).

- [ ] **Step 2: Write the failing test** — create `tests/test_elements.py` with exactly:

  ```python
  """Tests for Document.elements(): lazy sync iteration over PDF elements.

  Runs against the committed fixture PDFs in ``tests/fixtures/``. Requires the
  extension module to be built and installed (e.g. via maturin).
  """

  from pathlib import Path

  from pdfboss import Document, Element

  PHYSICAL_KINDS = {"header", "object", "xref", "trailer", "startxref", "eof"}
  LOGICAL_KINDS = {"page", "font", "image", "annotation", "content_op"}


  class TestSyncElements:
      def test_yields_element_instances(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          elements = list(doc.elements())
          assert elements
          assert all(isinstance(e, Element) for e in elements)

      def test_kinds_are_known(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          for element in doc.elements():
              assert element.kind in PHYSICAL_KINDS | LOGICAL_KINDS

      def test_elements_returns_a_lazy_iterator(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          it = doc.elements()
          assert iter(it) is it
          assert next(it).kind == "header"

      def test_each_call_returns_a_fresh_iterator(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          first = [e.kind for e in doc.elements()]
          second = [e.kind for e in doc.elements()]
          assert first == second

      def test_physical_layer_shape(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          kinds = [e.kind for e in doc.elements(logical=False)]
          assert kinds[0] == "header"
          assert "eof" in kinds
          assert set(kinds) <= PHYSICAL_KINDS

      def test_physical_spans_within_file(self, hello_pdf: Path) -> None:
          size = hello_pdf.stat().st_size
          doc = Document(str(hello_pdf))
          for element in doc.elements(logical=False):
              span = element.span
              assert span is not None
              start, end = span
              assert 0 <= start < end <= size

      def test_object_spans_start_at_the_object_header(self, hello_pdf: Path) -> None:
          raw = hello_pdf.read_bytes()
          doc = Document(str(hello_pdf))
          objects = [e for e in doc.elements(logical=False) if e.kind == "object"]
          assert objects
          for element in objects:
              num, gen = element.ref
              start, end = element.span
              assert raw[start:end].startswith(f"{num} {gen} obj".encode())
              assert b"endobj" in raw[start:end]

      def test_logical_layer_has_page_and_font(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          logical = list(doc.elements(physical=False))
          kinds = {e.kind for e in logical}
          assert "page" in kinds
          assert "font" in kinds
          pages = [e for e in logical if e.kind == "page"]
          assert [e.page for e in pages] == [0]
          assert all(e.span is None for e in pages)
          assert all(e.ref is not None for e in pages)

      def test_pages_filter(self, three_pages_pdf: Path) -> None:
          doc = Document(str(three_pages_pdf))
          pages = [
              e for e in doc.elements(physical=False, pages=[1]) if e.kind == "page"
          ]
          assert [e.page for e in pages] == [1]

      def test_content_ops_off_by_default_on_by_flag(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          default_kinds = {e.kind for e in doc.elements()}
          assert "content_op" not in default_kinds
          ops = [
              e
              for e in doc.elements(physical=False, content_ops=True)
              if e.kind == "content_op"
          ]
          assert ops
          assert all(e.page == 0 for e in ops)
          assert all(e.span is not None for e in ops)

      def test_keyword_only_arguments(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          try:
              doc.elements(False)
          except TypeError:
              pass
          else:
              raise AssertionError("elements() must reject positional arguments")

      def test_xref_stream_file_iterates(self, xref_stream_pdf: Path) -> None:
          doc = Document(str(xref_stream_pdf))
          kinds = [e.kind for e in doc.elements(logical=False)]
          assert kinds[0] == "header"
          assert "xref" in kinds
  ```

- [ ] **Step 3: Run test to verify it fails**
  ```bash
  uv run --no-sync pytest tests/test_elements.py -v
  ```
  Expected failure: collection error — `ImportError: cannot import name 'Element' from 'pdfboss'`.

- [ ] **Step 4: Write minimal implementation** — edit `crates/pdfboss-py/src/lib.rs`.

  (a) Replace the import block (current lines 10–19) with:

  ```rust
  use std::path::PathBuf;
  use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

  use pyo3::create_exception;
  use pyo3::exceptions::{PyException, PyIndexError, PyValueError};
  use pyo3::prelude::*;
  use pyo3::types::{PyBytes, PyDict};

  use pdfboss_core::elements::{Element as CoreElement, ElementOpts, Elements};
  use pdfboss_core::Document as CoreDocument;
  use pdfboss_core::Page as CorePage;
  ```

  (b) Add next to `pdf_err` (after line 31):

  ```rust
  /// Maps a core error to [`PdfError`] with the parse-layer prefix used by
  /// the element/async APIs.
  fn parse_err(e: pdfboss_core::Error) -> PyErr {
      PdfError::new_err(format!("parse: {e}"))
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
  ```

  (c) Append this method inside `#[pymethods] impl Document` (before the closing brace at line 203, after `extract_text`):

  ```rust
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
  ```

  (d) Insert after the `Page` `#[pymethods]` block (after line 306), before `#[pymodule]`:

  ```rust
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
      doc: Arc<SharedDocument>,
      iter: SharedElements,
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
  ```

  (e) Replace the `#[pymodule]` function (lines 308–315) with:

  ```rust
  #[pymodule]
  fn _pdfboss(m: &Bound<'_, PyModule>) -> PyResult<()> {
      m.add("__version__", env!("CARGO_PKG_VERSION"))?;
      m.add("PdfError", m.py().get_type::<PdfError>())?;
      m.add_class::<Document>()?;
      m.add_class::<Page>()?;
      m.add_class::<Element>()?;
      m.add_class::<ElementIter>()?;
      Ok(())
  }
  ```

  (f) In `mod tests`, replace `pyclasses_are_send_and_sync` (lines 358–367) with the version below and add the new `kind_str` test after it:

  ```rust
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
  ```

  (g) Replace `python/pdfboss/__init__.py` in full with:

  ```python
  """PDF parsing, text extraction and rendering in pure Rust."""

  from pdfboss._pdfboss import Document, Element, ElementIter, Page, PdfError, __version__

  __all__ = ["Document", "Element", "ElementIter", "Page", "PdfError", "__version__"]
  ```

- [ ] **Step 5: Run test to verify it passes**
  ```bash
  cd /Users/mohamed.tahrioui/private/pdfboss
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv run --no-sync maturin develop --uv
  uv run --no-sync pytest tests/test_elements.py -v
  uv run --no-sync pytest -q     # full suite still green
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-py
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check
  ```
  All PASS.

- [ ] **Step 6: Commit**
  ```bash
  git add crates/pdfboss-py/src/lib.rs python/pdfboss/__init__.py tests/test_elements.py
  git commit -m "feat(python): sync Document.elements() iterator with Element kind/span/ref/page"
  ```

### Task 2: `Element.value()` — lazy Rust `Object` → Python conversion

**Files:**
- Modify: `crates/pdfboss-py/src/lib.rs` (import block from Task 1; `#[pymethods] impl Element` block added in Task 1; new free functions next to `kind_str`)
- Test: `tests/test_elements.py` (append a class)

**Interfaces:**

Consumes (existing code actually read, `crates/pdfboss-core/src/object.rs` via the `pdfboss_core` root re-exports):

```rust
pub enum Object {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    String(Vec<u8>),
    Name(Name),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Stream),
    Ref(ObjRef),
}
pub struct Name(pub String);
pub struct ObjRef { pub num: u32, pub gen: u16 }
pub struct Stream { pub dict: Dict, pub data: Vec<u8> }
impl Dict { pub fn iter(&self) -> impl Iterator<Item = (&Name, &Object)>; }
```

Also consumes from plan 01 (spec): `XrefKind { Table, Stream }`, the `Element` variant payloads listed in Task 1, and `content::Op` (existing `crates/pdfboss-core/src/content.rs`, `#[derive(Debug, Clone, PartialEq)] pub enum Op`), rendered via its `Debug` impl.

Produces:

```python
class Element:
    def value(self) -> object: ...
    # lazy conversion: dict/list/str/bytes/int/float/bool/None
    # PDF names -> str, streams -> {"dict": ..., "length": int}
    # strings UTF-8 where valid else bytes, refs -> {"ref": (num, gen)}
```

Also produces (Rust, reused by Task 3's `get_object`): `fn object_to_py<'py>(py: Python<'py>, obj: &Object) -> PyResult<Bound<'py, PyAny>>` and `fn dict_to_py<'py>(py: Python<'py>, dict: &Dict) -> PyResult<Bound<'py, PyDict>>`.

- [ ] **Step 1: Write the failing test** — append to `tests/test_elements.py`:

  ```python
  class TestElementValues:
      def test_header_value_is_the_version_string(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          header = next(iter(doc.elements()))
          assert header.kind == "header"
          assert header.value() == doc.version

      def test_object_values_include_the_catalog_dict(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          values = [
              e.value() for e in doc.elements(logical=False) if e.kind == "object"
          ]
          catalogs = [
              v for v in values if isinstance(v, dict) and v.get("Type") == "Catalog"
          ]
          assert len(catalogs) == 1

      def test_refs_convert_to_ref_dicts(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          values = [
              e.value() for e in doc.elements(logical=False) if e.kind == "object"
          ]
          catalog = next(
              v for v in values if isinstance(v, dict) and v.get("Type") == "Catalog"
          )
          pages_ref = catalog["Pages"]
          assert set(pages_ref) == {"ref"}
          num, gen = pages_ref["ref"]
          assert isinstance(num, int)
          assert isinstance(gen, int)

      def test_stream_objects_convert_to_dict_and_length(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          values = [
              e.value() for e in doc.elements(logical=False) if e.kind == "object"
          ]
          streams = [
              v for v in values if isinstance(v, dict) and set(v) == {"dict", "length"}
          ]
          assert streams
          for stream in streams:
              assert isinstance(stream["dict"], dict)
              assert isinstance(stream["length"], int)
              assert stream["length"] >= 0

      def test_trailer_value_has_size_and_root(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          trailer = next(
              e for e in doc.elements(logical=False) if e.kind == "trailer"
          )
          value = trailer.value()
          assert isinstance(value["Size"], int)
          assert set(value["Root"]) == {"ref"}

      def test_startxref_eof_and_page_values(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          elements = list(doc.elements())
          startxref = next(e for e in elements if e.kind == "startxref")
          assert isinstance(startxref.value(), int)
          eof = next(e for e in elements if e.kind == "eof")
          assert eof.value() is None
          page = next(e for e in elements if e.kind == "page")
          assert page.value() is None

      def test_xref_value_reports_kind_and_entries(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          xref = next(e for e in doc.elements(logical=False) if e.kind == "xref")
          value = xref.value()
          assert value["kind"] in ("table", "stream")
          assert isinstance(value["entries"], int)
          assert value["entries"] > 0

      def test_font_value_has_subtype(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          font = next(e for e in doc.elements(physical=False) if e.kind == "font")
          value = font.value()
          assert isinstance(value["subtype"], str)
          assert value["subtype"]
          assert "base_font" in value

      def test_content_op_value_is_a_string(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          ops = [
              e
              for e in doc.elements(physical=False, content_ops=True)
              if e.kind == "content_op"
          ]
          assert ops
          assert all(isinstance(e.value(), str) and e.value() for e in ops)

      def test_value_is_repeatable(self, hello_pdf: Path) -> None:
          doc = Document(str(hello_pdf))
          for element in doc.elements(logical=False):
              assert element.value() == element.value()
  ```

- [ ] **Step 2: Run test to verify it fails**
  ```bash
  uv run --no-sync pytest tests/test_elements.py::TestElementValues -v
  ```
  Expected failure: `AttributeError: 'Element' object has no attribute 'value'` (or `'builtins.Element' object has no attribute 'value'`) in every test of the class.

- [ ] **Step 3: Write minimal implementation** — edit `crates/pdfboss-py/src/lib.rs`.

  (a) Replace the import block from Task 1 with:

  ```rust
  use std::path::PathBuf;
  use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

  use pyo3::create_exception;
  use pyo3::exceptions::{PyException, PyIndexError, PyValueError};
  use pyo3::prelude::*;
  use pyo3::types::{PyBytes, PyDict, PyList};
  use pyo3::IntoPyObjectExt;

  use pdfboss_core::elements::{Element as CoreElement, ElementOpts, Elements, XrefKind};
  use pdfboss_core::Document as CoreDocument;
  use pdfboss_core::Page as CorePage;
  use pdfboss_core::{Dict, Object};
  ```

  (b) Add after `kind_str`:

  ```rust
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
  ```

  (c) Replace the whole `#[pymethods] impl Element` block from Task 1 with this full new version (the four getters are unchanged; `value` is new):

  ```rust
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
              CoreElement::Header { version, .. } => {
                  version_string(*version).into_bound_py_any(py)
              }
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
              CoreElement::Eof { .. } | CoreElement::Page { .. } => {
                  Ok(py.None().into_bound(py))
              }
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
  ```

- [ ] **Step 4: Run test to verify it passes**
  ```bash
  cd /Users/mohamed.tahrioui/private/pdfboss
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv run --no-sync maturin develop --uv
  uv run --no-sync pytest tests/test_elements.py -v
  uv run --no-sync pytest -q
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-py
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check
  ```
  All PASS.

- [ ] **Step 5: Commit**
  ```bash
  git add crates/pdfboss-py/src/lib.rs tests/test_elements.py
  git commit -m "feat(python): Element.value() lazy object-to-Python conversion"
  ```

### Task 3: `AsyncDocument` — open/from_bytes, page_count/version, async metadata/get_object

**Files:**
- Modify: `crates/pdfboss-py/Cargo.toml` (`[dependencies]`, lines 13–17)
- Modify: `pyproject.toml` (`[dependency-groups]` lines 47–53, `[tool.pytest.ini_options]` lines 55–56)
- Modify: `crates/pdfboss-py/src/lib.rs` (import block from Task 2; `Document::metadata` getter at lines 128–149 of the original file; new items after `ElementIter`; `#[pymodule]`; `pyclasses_are_send_and_sync` unit test)
- Modify: `python/pdfboss/__init__.py` (whole file)
- Modify: `Cargo.lock` (regenerated by the build — commit it)
- Test: `tests/test_async.py` (new)

**Interfaces:**

Consumes (exact contract from plan 02 via the spec, `pdfboss-aio`; `http` feature ON for this crate):

```rust
pub struct AsyncDocument { /* Arc<dyn Backend>, parsed xref chain, trailer,
                              page tree index, objstm decode cache */ }
// AsyncDocument: Send + Sync + Clone (cheap Arc clone)

impl AsyncDocument {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self>;          // FileBackend
    pub async fn from_bytes(bytes: impl Into<Bytes>) -> Result<Self>;   // MemBackend
    #[cfg(feature = "http")]
    pub async fn open_url(url: impl IntoUrl) -> Result<Self>;           // HttpBackend
    pub async fn with_backend(backend: impl Backend) -> Result<Self>;

    pub fn version(&self) -> (u8, u8);
    pub fn page_count(&self) -> usize;
    pub async fn metadata(&self) -> Result<Metadata>;
    pub async fn get_object(&self, r: ObjRef) -> Result<Object>;
    pub async fn resolve(&self, obj: &Object) -> Result<Object>;
    pub async fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>>;
    pub async fn read_span(&self, span: Span) -> Result<Vec<u8>>;
    pub fn elements(&self, opts: ElementOpts) -> ElementStream<'_>;
}

pub enum Error {
    Core(pdfboss_core::Error),
    Io(std::io::Error),
    #[cfg(feature = "http")]
    Http { status: Option<u16>, msg: String },
    RangeUnsupported,      // server ignored Range requests
    TruncatedRead { offset: u64, wanted: usize, got: usize },
}
pub type Result<T> = std::result::Result<T, Error>;
```

Also consumes: `pdfboss_core::Metadata` (`crates/pdfboss-core/src/document.rs` lines 535–544: eight `Option<String>` fields `title`/`author`/`subject`/`keywords`/`creator`/`producer`/`creation_date`/`mod_date`), `pdfboss_core::ObjRef`, and Task 2's `object_to_py`.

Produces (Python surface later tasks rely on):

```python
class AsyncDocument:
    @staticmethod
    async def open(path: str | os.PathLike) -> "AsyncDocument": ...
    @staticmethod
    async def from_bytes(data: bytes) -> "AsyncDocument": ...
    def page_count(self) -> int: ...
    def version(self) -> str: ...
    async def metadata(self) -> dict[str, str]: ...
    async def get_object(self, num: int, gen: int = 0) -> object: ...
```

Also produces (Rust, consumed by Tasks 4–5): `struct AsyncDocument { inner: AioDocument }`, `fn aio_err(e: pdfboss_aio::Error) -> PyErr`, `fn metadata_dict(py: Python<'_>, meta: CoreMetadata) -> PyResult<Bound<'_, PyDict>>`.

- [ ] **Step 1: Add the Python test dependencies** — in `pyproject.toml`, replace lines 47–56 with:

  ```toml
  [dependency-groups]
  dev = [
      "maturin>=1.7",
      "pytest>=8",
      "pytest-asyncio>=0.24",
      # test_release_meta.py parses .github/workflows/release-please.yaml
      "pyyaml>=6",
  ]

  [tool.pytest.ini_options]
  testpaths = ["tests"]
  asyncio_default_fixture_loop_scope = "function"
  ```

  Then install: `cd /Users/mohamed.tahrioui/private/pdfboss && CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv sync`.

- [ ] **Step 2: Write the failing test** — create `tests/test_async.py` with exactly:

  ```python
  """Tests for AsyncDocument: async open, metadata and object fetch.

  Runs against the committed fixture PDFs in ``tests/fixtures/``. Requires the
  extension module to be built and installed (e.g. via maturin).
  """

  import asyncio
  from pathlib import Path

  import pytest

  from pdfboss import AsyncDocument, Document, PdfError


  class TestAsyncOpen:
      @pytest.mark.asyncio
      async def test_open_by_pathlike(self, hello_pdf: Path) -> None:
          doc = await AsyncDocument.open(hello_pdf)
          assert doc.page_count() == 1

      @pytest.mark.asyncio
      async def test_open_by_str(self, hello_pdf: Path) -> None:
          doc = await AsyncDocument.open(str(hello_pdf))
          assert doc.page_count() == 1

      @pytest.mark.asyncio
      async def test_from_bytes(self, hello_pdf: Path) -> None:
          doc = await AsyncDocument.from_bytes(hello_pdf.read_bytes())
          assert doc.page_count() == 1

      @pytest.mark.asyncio
      async def test_version_matches_sync(self, hello_pdf: Path) -> None:
          doc = await AsyncDocument.open(hello_pdf)
          assert doc.version() == Document(str(hello_pdf)).version

      @pytest.mark.asyncio
      async def test_xref_stream_file_opens(self, xref_stream_pdf: Path) -> None:
          doc = await AsyncDocument.open(xref_stream_pdf)
          assert doc.page_count() == 1

      @pytest.mark.asyncio
      async def test_missing_file_raises_prefixed_pdf_error(
          self, tmp_path: Path
      ) -> None:
          with pytest.raises(PdfError) as exc:
              await AsyncDocument.open(tmp_path / "missing.pdf")
          assert str(exc.value).startswith(("io:", "parse:"))

      @pytest.mark.asyncio
      async def test_garbage_bytes_raise_prefixed_pdf_error(self) -> None:
          with pytest.raises(PdfError) as exc:
              await AsyncDocument.from_bytes(b"not a pdf")
          assert str(exc.value).startswith(("parse:", "io:"))


  class TestAsyncDocumentQueries:
      @pytest.mark.asyncio
      async def test_metadata_matches_sync(self, hello_pdf: Path) -> None:
          doc = await AsyncDocument.open(hello_pdf)
          assert await doc.metadata() == Document(str(hello_pdf)).metadata

      @pytest.mark.asyncio
      async def test_get_object_fetches_the_catalog(self, hello_pdf: Path) -> None:
          trailer = next(
              e
              for e in Document(str(hello_pdf)).elements(logical=False)
              if e.kind == "trailer"
          )
          num, gen = trailer.value()["Root"]["ref"]
          doc = await AsyncDocument.open(hello_pdf)
          catalog = await doc.get_object(num, gen)
          assert catalog["Type"] == "Catalog"

      @pytest.mark.asyncio
      async def test_get_object_gen_defaults_to_zero(self, hello_pdf: Path) -> None:
          trailer = next(
              e
              for e in Document(str(hello_pdf)).elements(logical=False)
              if e.kind == "trailer"
          )
          num, gen = trailer.value()["Root"]["ref"]
          assert gen == 0
          doc = await AsyncDocument.open(hello_pdf)
          assert await doc.get_object(num) == await doc.get_object(num, gen)

      @pytest.mark.asyncio
      async def test_documents_run_concurrently(
          self, hello_pdf: Path, three_pages_pdf: Path
      ) -> None:
          docs = await asyncio.gather(
              AsyncDocument.open(hello_pdf),
              AsyncDocument.open(three_pages_pdf),
          )
          assert [d.page_count() for d in docs] == [1, 3]
  ```

- [ ] **Step 3: Run test to verify it fails**
  ```bash
  uv run --no-sync pytest tests/test_async.py -v
  ```
  Expected failure: collection error — `ImportError: cannot import name 'AsyncDocument' from 'pdfboss'`.

- [ ] **Step 4: Add the Rust dependencies** — replace `[dependencies]` in `crates/pdfboss-py/Cargo.toml` (lines 13–17) with:

  ```toml
  [dependencies]
  pdfboss-core = { path = "../pdfboss-core" }
  pdfboss-text = { path = "../pdfboss-text" }
  pdfboss-render = { path = "../pdfboss-render" }
  # `http` is enabled here so every wheel/sdist build ships open_url support.
  pdfboss-aio = { path = "../pdfboss-aio", features = ["http"] }
  pyo3 = { version = "0.25", features = ["abi3-py312"] }
  pyo3-async-runtimes = { version = "0.25", features = ["tokio-runtime"] }
  tokio = { version = "1", features = ["sync"] }
  futures-util = "0.3"
  ```

  `pyo3-async-runtimes`'s tokio flavor lazily initializes ONE global multi-thread tokio runtime on first use; every `future_into_py` call below runs on it — no per-call or per-class runtimes.

- [ ] **Step 5: Write minimal implementation** — edit `crates/pdfboss-py/src/lib.rs`.

  (a) Replace the import block from Task 2 with:

  ```rust
  use std::path::PathBuf;
  use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

  use pyo3::create_exception;
  use pyo3::exceptions::{PyException, PyIndexError, PyValueError};
  use pyo3::prelude::*;
  use pyo3::types::{PyBytes, PyDict, PyList};
  use pyo3::IntoPyObjectExt;

  use pdfboss_aio::AsyncDocument as AioDocument;
  use pdfboss_core::elements::{Element as CoreElement, ElementOpts, Elements, XrefKind};
  use pdfboss_core::Document as CoreDocument;
  use pdfboss_core::Metadata as CoreMetadata;
  use pdfboss_core::Page as CorePage;
  use pdfboss_core::{Dict, ObjRef, Object};
  ```

  (b) Add after `parse_err`:

  ```rust
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
          AioError::RangeUnsupported => {
              "http: server does not support Range requests".to_string()
          }
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
  ```

  (c) Replace the `Document::metadata` getter (original lines 128–149) with this behavior-identical version that reuses the helper:

  ```rust
      /// Document metadata; only keys present in the file are included.
      #[getter]
      fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
          let meta = self.inner.lock().metadata();
          metadata_dict(py, meta)
      }
  ```

  (d) Insert after the `ElementIter` `#[pymethods]` block, before `#[pymodule]`:

  ```rust
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
      /// AsyncDocument. The whole file is never read eagerly.
      #[staticmethod]
      fn open(py: Python<'_>, path: PathBuf) -> PyResult<Bound<'_, PyAny>> {
          pyo3_async_runtimes::tokio::future_into_py(py, async move {
              let inner = AioDocument::open(path).await.map_err(aio_err)?;
              Ok(AsyncDocument { inner })
          })
      }

      /// Loads a PDF from bytes already in memory. Coroutine resolving to
      /// an AsyncDocument.
      #[staticmethod]
      fn from_bytes(py: Python<'_>, data: Vec<u8>) -> PyResult<Bound<'_, PyAny>> {
          pyo3_async_runtimes::tokio::future_into_py(py, async move {
              let inner = AioDocument::from_bytes(data).await.map_err(aio_err)?;
              Ok(AsyncDocument { inner })
          })
      }

      /// Number of pages in the document.
      fn page_count(&self) -> usize {
          self.inner.page_count()
      }

      /// PDF version from the file header, e.g. "1.7".
      fn version(&self) -> String {
          version_string(self.inner.version())
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
  }
  ```

  (e) Replace the `#[pymodule]` function with:

  ```rust
  #[pymodule]
  fn _pdfboss(m: &Bound<'_, PyModule>) -> PyResult<()> {
      m.add("__version__", env!("CARGO_PKG_VERSION"))?;
      m.add("PdfError", m.py().get_type::<PdfError>())?;
      m.add_class::<Document>()?;
      m.add_class::<Page>()?;
      m.add_class::<Element>()?;
      m.add_class::<ElementIter>()?;
      m.add_class::<AsyncDocument>()?;
      Ok(())
  }
  ```

  (f) In `mod tests`, replace `pyclasses_are_send_and_sync` with:

  ```rust
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
      }
  ```

  (g) Replace `python/pdfboss/__init__.py` in full with:

  ```python
  """PDF parsing, text extraction and rendering in pure Rust."""

  from pdfboss._pdfboss import (
      AsyncDocument,
      Document,
      Element,
      ElementIter,
      Page,
      PdfError,
      __version__,
  )

  __all__ = [
      "AsyncDocument",
      "Document",
      "Element",
      "ElementIter",
      "Page",
      "PdfError",
      "__version__",
  ]
  ```

- [ ] **Step 6: Run test to verify it passes**
  ```bash
  cd /Users/mohamed.tahrioui/private/pdfboss
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv run --no-sync maturin develop --uv
  uv run --no-sync pytest tests/test_async.py -v
  uv run --no-sync pytest -q
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-py
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check
  ```
  All PASS.

- [ ] **Step 7: Commit**
  ```bash
  git add crates/pdfboss-py/Cargo.toml Cargo.lock pyproject.toml uv.lock \
      crates/pdfboss-py/src/lib.rs python/pdfboss/__init__.py tests/test_async.py
  git commit -m "feat(python): AsyncDocument with open/from_bytes/metadata/get_object"
  ```
  (Skip `uv.lock` in the `git add` if the repo has none after `uv sync`; add whatever lockfiles actually changed.)

### Task 4: `AsyncDocument.elements()` — `AsyncElementIter` with `__aiter__`/`__anext__`

**Files:**
- Modify: `crates/pdfboss-py/src/lib.rs` (import block from Task 3; new items after the `AsyncDocument` `#[pymethods]` block; one method appended inside `#[pymethods] impl AsyncDocument` — pyo3 allows only one `#[pymethods]` block per class without the `multiple-pymethods` feature, so extend the existing block; `#[pymodule]`; `pyclasses_are_send_and_sync`)
- Modify: `python/pdfboss/__init__.py` (whole file)
- Test: `tests/test_async.py` (append)

**Interfaces:**

Consumes (exact contract from plan 02 via the spec):

```rust
impl AsyncDocument {
    pub fn elements(&self, opts: ElementOpts) -> ElementStream<'_>;
}

pub struct ElementStream<'a> { /* async state machine mirroring core's Elements */ }
impl<'a> futures_core::Stream for ElementStream<'a> {
    type Item = Result<Element>;   // pdfboss_aio::Result<pdfboss_core::elements::Element>
}
// ElementStream<'a>: Send (pinned by the spec); same ordering and salvage
// semantics as the sync iterator.
```

Also consumes: Task 1's `Element` pyclass and `ElementOpts`, Task 3's `AsyncDocument`/`aio_err`, `futures_util::StreamExt::next` (works on `Pin<Box<S>>` for any `S: Stream`, no `Unpin` assumption about `ElementStream` itself).

Produces:

```python
class AsyncElementIter:
    def __aiter__(self) -> "AsyncElementIter": ...
    async def __anext__(self) -> Element: ...   # raises StopAsyncIteration when done

class AsyncDocument:
    def elements(self, *, physical: bool = True, logical: bool = True,
                 pages: list[int] | None = None,
                 content_ops: bool = False) -> AsyncIterator[Element]: ...
```

- [ ] **Step 1: Write the failing test** — in `tests/test_async.py`, change the import line

  ```python
  from pdfboss import AsyncDocument, Document, PdfError
  ```

  to

  ```python
  from pdfboss import AsyncDocument, Document, Element, PdfError
  ```

  and append:

  ```python
  def element_key(element: Element) -> tuple[object, ...]:
      """A comparable identity for an element (everything but value())."""
      return (element.kind, element.span, element.ref, element.page)


  class TestAsyncElements:
      @pytest.mark.asyncio
      async def test_async_for_yields_elements(self, hello_pdf: Path) -> None:
          doc = await AsyncDocument.open(hello_pdf)
          kinds = []
          async for element in doc.elements():
              assert isinstance(element, Element)
              kinds.append(element.kind)
          assert kinds[0] == "header"
          assert "page" in kinds

      @pytest.mark.asyncio
      @pytest.mark.parametrize(
          "name", ["hello.pdf", "three-pages.pdf", "shapes.pdf", "xref-stream.pdf"]
      )
      async def test_parity_with_sync_elements(
          self, fixtures_dir: Path, name: str
      ) -> None:
          path = fixtures_dir / name
          expected = [
              element_key(e) for e in Document(str(path)).elements(content_ops=True)
          ]
          doc = await AsyncDocument.open(path)
          got = []
          async for element in doc.elements(content_ops=True):
              got.append(element_key(element))
          assert got == expected

      @pytest.mark.asyncio
      async def test_values_match_sync(self, hello_pdf: Path) -> None:
          expected = [e.value() for e in Document(str(hello_pdf)).elements()]
          doc = await AsyncDocument.open(hello_pdf)
          got = []
          async for element in doc.elements():
              got.append(element.value())
          assert got == expected

      @pytest.mark.asyncio
      async def test_filters_pass_through(self, three_pages_pdf: Path) -> None:
          doc = await AsyncDocument.open(three_pages_pdf)
          pages = []
          async for element in doc.elements(physical=False, pages=[1]):
              if element.kind == "page":
                  pages.append(element.page)
          assert pages == [1]

      @pytest.mark.asyncio
      async def test_event_loop_stays_responsive(self, three_pages_pdf: Path) -> None:
          doc = await AsyncDocument.open(three_pages_pdf)
          ticks = 0

          async def ticker() -> None:
              nonlocal ticks
              while True:
                  ticks += 1
                  await asyncio.sleep(0)

          task = asyncio.create_task(ticker())
          try:
              count = 0
              async for element in doc.elements(content_ops=True):
                  count += 1
          finally:
              task.cancel()
          assert count > 0
          assert ticks > 0
  ```

- [ ] **Step 2: Run test to verify it fails**
  ```bash
  uv run --no-sync pytest tests/test_async.py::TestAsyncElements -v
  ```
  Expected failure: `AttributeError: 'AsyncDocument' object has no attribute 'elements'` in every test of the class.

- [ ] **Step 3: Write minimal implementation** — edit `crates/pdfboss-py/src/lib.rs`.

  (a) Replace the import block from Task 3 with:

  ```rust
  use std::path::PathBuf;
  use std::pin::Pin;
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
  use pdfboss_core::{Dict, ObjRef, Object};
  ```

  (b) Append this method inside the existing `#[pymethods] impl AsyncDocument` block (after `get_object`):

  ```rust
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
              state: Arc::new(tokio::sync::Mutex::new(ElementStreamHolder::new(
                  self.inner.clone(),
                  opts,
              ))),
          }
      }
  ```

  (c) Insert after the `AsyncDocument` `#[pymethods]` block, before `#[pymodule]`:

  ```rust
  /// The stream state behind `AsyncDocument.elements()`.
  ///
  /// `stream` borrows `doc` with its lifetime extended to `'static`. That
  /// is sound because:
  ///
  /// - `doc` is boxed, so the borrowed `AioDocument` sits at a stable heap
  ///   address even when the holder itself moves, and
  /// - the fields are declared `stream` first, `doc` second, so the stream
  ///   drops before the document it borrows.
  struct ElementStreamHolder {
      stream: Pin<Box<ElementStream<'static>>>,
      doc: Box<AioDocument>,
  }

  impl ElementStreamHolder {
      fn new(doc: AioDocument, opts: ElementOpts) -> Self {
          let doc = Box::new(doc);
          // SAFETY: see the type-level comment — the box keeps the document
          // at a stable address for the holder's whole lifetime, and field
          // declaration order guarantees the stream drops first.
          let borrowed: &'static AioDocument =
              unsafe { std::mem::transmute::<&AioDocument, &'static AioDocument>(&doc) };
          ElementStreamHolder {
              stream: Box::pin(borrowed.elements(opts)),
              doc,
          }
      }
  }

  /// Async iterator over a document's elements, returned by
  /// `AsyncDocument.elements()`. Each `__anext__` is a coroutine driving
  /// the Rust element stream on the tokio runtime, so the asyncio loop is
  /// never blocked.
  #[pyclass(frozen)]
  struct AsyncElementIter {
      state: Arc<tokio::sync::Mutex<ElementStreamHolder>>,
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
          let state = Arc::clone(&self.state);
          pyo3_async_runtimes::tokio::future_into_py(py, async move {
              let mut holder = state.lock().await;
              match holder.stream.next().await {
                  Some(Ok(element)) => Ok(Element { inner: element }),
                  Some(Err(e)) => Err(aio_err(e)),
                  None => Err(PyStopAsyncIteration::new_err("element stream exhausted")),
              }
          })
      }
  }
  ```

  Note on `transmute(&doc)`: `&doc` is `&Box<AioDocument>`, which auto-derefs to `&AioDocument` at the transmute's typed input — if the compiler complains, write `&*doc` explicitly. `doc` is a plain owning field kept only so the borrow stays alive; if `-D warnings` flags it as never read, silence exactly that field with `#[allow(dead_code)] // owner of the stream's borrow` on the `doc` field (an attribute, not an underscore rename).

  (d) Replace the `#[pymodule]` function with:

  ```rust
  #[pymodule]
  fn _pdfboss(m: &Bound<'_, PyModule>) -> PyResult<()> {
      m.add("__version__", env!("CARGO_PKG_VERSION"))?;
      m.add("PdfError", m.py().get_type::<PdfError>())?;
      m.add_class::<Document>()?;
      m.add_class::<Page>()?;
      m.add_class::<Element>()?;
      m.add_class::<ElementIter>()?;
      m.add_class::<AsyncDocument>()?;
      m.add_class::<AsyncElementIter>()?;
      Ok(())
  }
  ```

  (e) In `mod tests`, replace `pyclasses_are_send_and_sync` with:

  ```rust
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
  ```

  (f) Replace `python/pdfboss/__init__.py` in full with:

  ```python
  """PDF parsing, text extraction and rendering in pure Rust."""

  from pdfboss._pdfboss import (
      AsyncDocument,
      AsyncElementIter,
      Document,
      Element,
      ElementIter,
      Page,
      PdfError,
      __version__,
  )

  __all__ = [
      "AsyncDocument",
      "AsyncElementIter",
      "Document",
      "Element",
      "ElementIter",
      "Page",
      "PdfError",
      "__version__",
  ]
  ```

- [ ] **Step 4: Run test to verify it passes**
  ```bash
  cd /Users/mohamed.tahrioui/private/pdfboss
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv run --no-sync maturin develop --uv
  uv run --no-sync pytest tests/test_async.py -v
  uv run --no-sync pytest -q
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-py
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check
  ```
  All PASS.

- [ ] **Step 5: Commit**
  ```bash
  git add crates/pdfboss-py/src/lib.rs python/pdfboss/__init__.py tests/test_async.py
  git commit -m "feat(python): async element streaming via AsyncDocument.elements()"
  ```

### Task 5: `AsyncDocument.open_url()` against an in-test Range-serving HTTP server

**Files:**
- Modify: `crates/pdfboss-py/src/lib.rs` (one method appended inside `#[pymethods] impl AsyncDocument`, after `from_bytes`)
- Test: `tests/test_async.py` (append; also extend the imports)

**Interfaces:**

Consumes (exact contract from plan 02 via the spec):

```rust
impl AsyncDocument {
    #[cfg(feature = "http")]
    pub async fn open_url(url: impl IntoUrl) -> Result<Self>;   // HttpBackend
}
// HttpBackend: len via HEAD/Content-Length, read_at via `Range: bytes=`
// requests. Fails with a clear error if the server ignores Range
// (Error::RangeUnsupported / Error::Http).
```

The `http` feature is already on via Task 3's dependency line, so no `cfg` is needed in the bindings.

Produces:

```python
class AsyncDocument:
    @staticmethod
    async def open_url(url: str) -> "AsyncDocument": ...
```

- [ ] **Step 1: Write the failing test** — in `tests/test_async.py`, replace the import section at the top of the file (everything between the module docstring and `from pdfboss import ...`, inclusive of that line) with:

  ```python
  import asyncio
  import threading
  from collections.abc import Iterator
  from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
  from pathlib import Path

  import pytest

  from pdfboss import AsyncDocument, Document, Element, PdfError
  ```

  Then append to the end of the file. stdlib `http.server` does NOT support Range natively, so the handler implements it in full:

  ```python
  class RangeRequestHandler(BaseHTTPRequestHandler):
      """Serves one in-memory payload with HTTP Range support.

      Handles a single byte-range per request: ``bytes=start-end``,
      ``bytes=start-`` and the suffix form ``bytes=-length``. Multi-range
      requests and unparsable specs get a 416.
      """

      protocol_version = "HTTP/1.1"
      payload: bytes = b""

      def log_message(self, format: str, *args: object) -> None:
          """Keep pytest output clean (stdlib hook, stdlib signature)."""

      def send_full_headers(self) -> bytes:
          data = type(self).payload
          self.send_response(200)
          self.send_header("Content-Length", str(len(data)))
          self.send_header("Accept-Ranges", "bytes")
          self.send_header("Content-Type", "application/pdf")
          self.end_headers()
          return data

      def parse_range(self, spec: str, size: int) -> tuple[int, int] | None:
          """Parses one byte-range spec into inclusive (start, end) bounds."""
          first, sep, last = spec.strip().partition("-")
          if not sep or "," in spec:
              return None
          if first == "":
              if not last.isdigit() or int(last) == 0:
                  return None
              return (max(size - int(last), 0), size - 1)
          if not first.isdigit():
              return None
          start = int(first)
          if start >= size:
              return None
          if last == "":
              return (start, size - 1)
          if not last.isdigit():
              return None
          return (start, min(int(last), size - 1))

      def do_HEAD(self) -> None:
          self.send_full_headers()

      def do_GET(self) -> None:
          data = type(self).payload
          header = self.headers.get("Range")
          if header is None:
              self.wfile.write(self.send_full_headers())
              return
          unit, sep, spec = header.partition("=")
          bounds = None
          if sep and unit.strip() == "bytes":
              bounds = self.parse_range(spec, len(data))
          if bounds is None:
              self.send_response(416)
              self.send_header("Content-Range", f"bytes */{len(data)}")
              self.send_header("Content-Length", "0")
              self.end_headers()
              return
          start, end = bounds
          body = data[start : end + 1]
          self.send_response(206)
          self.send_header("Content-Range", f"bytes {start}-{end}/{len(data)}")
          self.send_header("Content-Length", str(len(body)))
          self.send_header("Accept-Ranges", "bytes")
          self.send_header("Content-Type", "application/pdf")
          self.end_headers()
          self.wfile.write(body)


  class NoRangeRequestHandler(BaseHTTPRequestHandler):
      """Ignores Range entirely: always answers 200 with the full payload."""

      protocol_version = "HTTP/1.1"
      payload: bytes = b""

      def log_message(self, format: str, *args: object) -> None:
          """Keep pytest output clean (stdlib hook, stdlib signature)."""

      def do_HEAD(self) -> None:
          self.send_response(200)
          self.send_header("Content-Length", str(len(type(self).payload)))
          self.send_header("Content-Type", "application/pdf")
          self.end_headers()

      def do_GET(self) -> None:
          data = type(self).payload
          self.send_response(200)
          self.send_header("Content-Length", str(len(data)))
          self.send_header("Content-Type", "application/pdf")
          self.end_headers()
          self.wfile.write(data)


  def serve(handler: type[BaseHTTPRequestHandler]) -> Iterator[str]:
      """Runs `handler` on a background ThreadingHTTPServer; yields the URL."""
      server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
      thread = threading.Thread(target=server.serve_forever, daemon=True)
      thread.start()
      try:
          yield f"http://127.0.0.1:{server.server_address[1]}/doc.pdf"
      finally:
          server.shutdown()
          thread.join()
          server.server_close()


  @pytest.fixture
  def range_server(hello_pdf: Path) -> Iterator[str]:
      handler = type(
          "HelloRangeHandler",
          (RangeRequestHandler,),
          {"payload": hello_pdf.read_bytes()},
      )
      yield from serve(handler)


  @pytest.fixture
  def no_range_server(hello_pdf: Path) -> Iterator[str]:
      handler = type(
          "HelloNoRangeHandler",
          (NoRangeRequestHandler,),
          {"payload": hello_pdf.read_bytes()},
      )
      yield from serve(handler)


  class TestOpenUrl:
      @pytest.mark.asyncio
      async def test_open_url_over_range_requests(
          self, range_server: str, hello_pdf: Path
      ) -> None:
          doc = await AsyncDocument.open_url(range_server)
          assert doc.page_count() == 1
          assert doc.version() == Document(str(hello_pdf)).version

      @pytest.mark.asyncio
      async def test_open_url_element_parity(
          self, range_server: str, hello_pdf: Path
      ) -> None:
          expected = [element_key(e) for e in Document(str(hello_pdf)).elements()]
          doc = await AsyncDocument.open_url(range_server)
          got = []
          async for element in doc.elements():
              got.append(element_key(element))
          assert got == expected

      @pytest.mark.asyncio
      async def test_get_object_over_http(
          self, range_server: str, hello_pdf: Path
      ) -> None:
          trailer = next(
              e
              for e in Document(str(hello_pdf)).elements(logical=False)
              if e.kind == "trailer"
          )
          num, gen = trailer.value()["Root"]["ref"]
          doc = await AsyncDocument.open_url(range_server)
          catalog = await doc.get_object(num, gen)
          assert catalog["Type"] == "Catalog"

      @pytest.mark.asyncio
      async def test_server_without_range_support_raises_http_error(
          self, no_range_server: str
      ) -> None:
          with pytest.raises(PdfError) as exc:
              await AsyncDocument.open_url(no_range_server)
          assert str(exc.value).startswith("http:")

      @pytest.mark.asyncio
      async def test_unreachable_url_raises_http_error(self) -> None:
          with pytest.raises(PdfError) as exc:
              await AsyncDocument.open_url("http://127.0.0.1:9/doc.pdf")
          assert str(exc.value).startswith("http:")
  ```

- [ ] **Step 2: Run test to verify it fails**
  ```bash
  uv run --no-sync pytest tests/test_async.py::TestOpenUrl -v
  ```
  Expected failure: `AttributeError: type object 'AsyncDocument' has no attribute 'open_url'` in every test of the class.

- [ ] **Step 3: Write minimal implementation** — in `crates/pdfboss-py/src/lib.rs`, add this method inside `#[pymethods] impl AsyncDocument`, directly after `from_bytes`:

  ```rust
      /// Opens a PDF over HTTP using range requests; the whole file is
      /// never downloaded. The server must honor `Range` (a server that
      /// ignores it raises PdfError with an "http:" message). Coroutine
      /// resolving to an AsyncDocument.
      #[staticmethod]
      fn open_url(py: Python<'_>, url: String) -> PyResult<Bound<'_, PyAny>> {
          pyo3_async_runtimes::tokio::future_into_py(py, async move {
              let inner = AioDocument::open_url(url).await.map_err(aio_err)?;
              Ok(AsyncDocument { inner })
          })
      }
  ```

- [ ] **Step 4: Run test to verify it passes**
  ```bash
  cd /Users/mohamed.tahrioui/private/pdfboss
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv run --no-sync maturin develop --uv
  uv run --no-sync pytest tests/test_async.py -v
  uv run --no-sync pytest -q
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-py
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check
  ```
  All PASS.

- [ ] **Step 5: Commit**
  ```bash
  git add crates/pdfboss-py/src/lib.rs tests/test_async.py
  git commit -m "feat(python): AsyncDocument.open_url over HTTP range requests"
  ```

### Task 6: Type stubs — extend `python/pdfboss/_pdfboss.pyi`

**Files:**
- Modify: `python/pdfboss/_pdfboss.pyi` (whole file; current file is 99 lines)
- Test: `tests/test_stubs.py` (new)

**Interfaces:**

Consumes: the Python surface produced by Tasks 1–5 (already exported from `python/pdfboss/__init__.py` in Tasks 1, 3 and 4).

Produces: the stub surface below — the `Element`, `Document.elements`, and `AsyncDocument` signatures match the spec's `.pyi` block exactly (types, parameter names, defaults, keyword-only markers).

- [ ] **Step 1: Write the failing test** — create `tests/test_stubs.py` with exactly:

  ```python
  """The type stubs must cover every name and method the package exports."""

  from pathlib import Path

  import pdfboss

  STUB = Path(__file__).parent.parent / "python" / "pdfboss" / "_pdfboss.pyi"


  def test_stub_declares_every_exported_class() -> None:
      stub = STUB.read_text()
      for name in pdfboss.__all__:
          if name == "__version__":
              continue
          assert f"class {name}" in stub, f"missing stub for {name}"


  def test_stub_declares_the_element_and_async_surface() -> None:
      stub = STUB.read_text()
      assert "def elements(" in stub
      assert "def value(self) -> object" in stub
      assert "async def open(path: str | os.PathLike)" in stub
      assert "async def open_url(url: str)" in stub
      assert "async def from_bytes(data: bytes)" in stub
      assert "async def metadata(self) -> dict[str, str]" in stub
      assert "async def get_object(self, num: int, gen: int = 0)" in stub
      assert "Iterator[Element]" in stub
      assert "AsyncIterator[Element]" in stub
  ```

- [ ] **Step 2: Run test to verify it fails**
  ```bash
  uv run --no-sync pytest tests/test_stubs.py -v
  ```
  Expected failure: `AssertionError: missing stub for AsyncDocument` (first missing name reported; `Element`, `ElementIter`, `AsyncElementIter` are missing too), and the surface test fails on `async def open(...)`.

- [ ] **Step 3: Write minimal implementation** — replace `python/pdfboss/_pdfboss.pyi` in full with:

  ```python
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

  class AsyncDocument:
      """A PDF document opened for async I/O.

      Constructors and data-fetching methods are coroutines driven by one
      global multi-thread tokio runtime; ``page_count``/``version`` are
      sync because the open flow already parsed the xref chain and page
      tree. The whole file is never read eagerly — file and HTTP backends
      fetch only the byte ranges they need.
      """

      @staticmethod
      async def open(path: str | os.PathLike) -> "AsyncDocument": ...
      @staticmethod
      async def open_url(url: str) -> "AsyncDocument": ...
      @staticmethod
      async def from_bytes(data: bytes) -> "AsyncDocument": ...
      def page_count(self) -> int: ...
      def version(self) -> str: ...
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
  ```

- [ ] **Step 4: Run test to verify it passes**
  ```bash
  cd /Users/mohamed.tahrioui/private/pdfboss
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv run --no-sync maturin develop --uv
  uv run --no-sync pytest tests/test_stubs.py -v
  uv run --no-sync pytest -q
  ```
  All PASS. (The `maturin develop` re-run installs the updated `.pyi` alongside the extension.)

- [ ] **Step 5: Commit**
  ```bash
  git add python/pdfboss/_pdfboss.pyi tests/test_stubs.py
  git commit -m "feat(python): type stubs for Element, elements() and AsyncDocument"
  ```

### Task 7: Python CI installs pytest-asyncio; final verification

**Files:**
- Modify: `.github/workflows/python-ci.yml` (line 43–44: the "Install package" step of the `pytest` job)
- Test: `tests/test_python_ci.py` (new)

**Interfaces:**

Consumes: the `pytest` job of `.github/workflows/python-ci.yml` (steps read in this repo: checkout, rust-toolchain, rust-cache, setup-python, `Install package (builds extension via maturin backend)` running `pip install . pytest pyyaml`, then `pytest -q`). Test style mirrors `tests/test_release_meta.py`, which parses a workflow file with `pyyaml`.

Produces: a CI pytest job that can run the `@pytest.mark.asyncio` tests added in Tasks 3–5 (without pytest-asyncio they fail with "async def functions are not natively supported").

- [ ] **Step 1: Write the failing test** — create `tests/test_python_ci.py` with exactly:

  ```python
  """Pins the python-ci workflow contract the async binding tests rely on."""

  from pathlib import Path

  import yaml

  WORKFLOW = (
      Path(__file__).parent.parent / ".github" / "workflows" / "python-ci.yml"
  )


  def test_pytest_job_installs_pytest_asyncio() -> None:
      workflow = yaml.safe_load(WORKFLOW.read_text())
      steps = workflow["jobs"]["pytest"]["steps"]
      install = next(
          step for step in steps if step.get("name", "").startswith("Install package")
      )
      assert "pytest-asyncio" in install["run"]
  ```

- [ ] **Step 2: Run test to verify it fails**
  ```bash
  uv run --no-sync pytest tests/test_python_ci.py -v
  ```
  Expected failure: `AssertionError` — `pytest-asyncio` is not in `pip install . pytest pyyaml`.

- [ ] **Step 3: Write minimal implementation** — in `.github/workflows/python-ci.yml`, replace the install step of the `pytest` job:

  ```yaml
        - name: Install package (builds extension via maturin backend)
          run: pip install . pytest pytest-asyncio pyyaml
  ```

  (Only the `run:` line changes; everything else in the workflow stays untouched. The wheel `build` job needs no change — the `http` feature rides the `pdfboss-aio` dependency line from Task 3.)

- [ ] **Step 4: Run test to verify it passes**
  ```bash
  uv run --no-sync pytest tests/test_python_ci.py -v
  ```
  PASS.

- [ ] **Step 5: Full-suite final verification** — everything this plan delivered, end to end:
  ```bash
  cd /Users/mohamed.tahrioui/private/pdfboss
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target uv run --no-sync maturin develop --uv
  uv run --no-sync pytest -q
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test --workspace
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings
  CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check
  RUSTDOCFLAGS="-D warnings" CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo doc --workspace --no-deps
  ```
  All PASS. Existing tests (`tests/test_pdfboss.py`, `tests/test_release_meta.py`) must be untouched and green.

- [ ] **Step 6: Commit**
  ```bash
  git add .github/workflows/python-ci.yml tests/test_python_ci.py
  git commit -m "ci(python): install pytest-asyncio for the async binding tests"
  ```

## Spec coverage map (self-review)

Every pdfboss-py bullet of `docs/superpowers/specs/2026-07-24-pdf-element-explorer-design.md`:

| Spec requirement | Task |
| --- | --- |
| `Element` pyclass with `kind`/`span`/`ref`/`page` | Task 1 |
| `Element.value()` lazy conversion (names→str, streams→`{"dict","length"}`, strings UTF-8-else-bytes) | Task 2 |
| Sync `Document.elements(*, physical, logical, pages, content_ops)` → iterator; `__next__` releases the GIL | Task 1 |
| `AsyncDocument` via pyo3-async-runtimes on one global multi-thread tokio runtime | Task 3 |
| Staticmethod coroutines `open`/`from_bytes` | Task 3 |
| Staticmethod coroutine `open_url` (http feature, on for wheels) | Tasks 3 (feature) + 5 (method) |
| Sync `page_count()`/`version()`; async `metadata()`/`get_object(num, gen=0)` | Task 3 |
| `elements(...)` → `__aiter__`/`__anext__`, coroutine per item, asyncio loop never blocked | Task 4 |
| All errors → existing `PdfError`, layer-prefixed (`"http: …"`, `"parse: …"`, `"io: …"`) | Tasks 1 (`parse_err`) + 3 (`aio_err`) |
| `_pdfboss.pyi` mirrors the spec surface; `__init__.py` exports | Tasks 1/3/4 (exports) + 6 (stubs) |
| Wheels build `pdfboss-aio` with `http` enabled (dep line) | Task 3 |
| Python CI gains pytest-asyncio | Tasks 3 (dev dep) + 7 (workflow) |
| Tests: sync elements on fixtures; sync-vs-async parity; asyncio open/async-for; `open_url` vs a local Range server (incl. Range-refusing server) | Tasks 1/2 (sync), 4 (parity/async-for), 5 (Range server) |

