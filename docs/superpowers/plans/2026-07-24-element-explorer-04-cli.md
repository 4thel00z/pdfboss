# pdfboss fq-style CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `pdfboss json`, `pdfboss hex`, and `pdfboss q` subcommands: a JSON value-tree dump of a whole PDF, a hexyl-style hexdump with element-aware selectors and annotations, and a jq-programmable query interface over the value tree with query-to-bytes hexdumping.

**Architecture:** All new code lives in `pdfboss-cli` only: `src/input.rs` abstracts local files (sync `Document` fast path) vs. `http(s)` URLs (`pdfboss-aio` `AsyncDocument` on a small single-thread tokio runtime); `src/q/value.rs` converts the element sequence from `pdfboss_core::elements` into a `serde_json::Value` tree in the spec's pinned wire format; `src/json.rs`, `src/hexdump.rs`, and `src/q/run.rs` implement the three subcommands on top. The jq engine is jaq (`jaq-core`/`jaq-std`/`jaq-json`), compiled per invocation and run over the materialized tree.

**Tech Stack:** Rust 2021, clap 4 (derive), serde_json (preserve_order), base64, jaq-core 2 + jaq-std 2 + jaq-json 1, tokio (current-thread rt), futures-core, pdfboss-core (`elements`, `pretty`), pdfboss-aio (`http` feature), pdfboss-testkit (dev).

## Global Constraints

- **Cleanroom rule (from the 2026-07-12 spec, unchanged):** everything is implemented purely from ISO 32000. Do not copy, port, translate, or reference the source code, identifiers, comments, or documentation of any existing PDF library, in any language. **Never name any other PDF library anywhere** — not in code, comments, docs, README, tests, commit messages, or plan prose. jq, jaq, fq, and hexyl are not PDF libraries; naming them is fine.
- **jq engine and `serde_json` are confined to `pdfboss-cli`.** They never appear in `pdfboss-core` (or any other crate). All JSON conversion is the CLI's job.
- **`pdfboss-core` gains zero new dependencies** and zero changes in this plan. This plan only consumes the APIs plans 01/02 added.
- **Existing subcommands (`info`, `text`, `render`, `obj`) stay untouched** in behavior; their tests in `crates/pdfboss-cli/tests/cli.rs` must keep passing unmodified.
- **Never create underscore-prefixed Rust identifiers** (no `_foo` functions, methods, fields, or variables). JSON wire-format keys such as `"_span"`, `"_ref"`, `"_kind"`, `"_objstm"`, `"_r"`, `"_bytes"`, `"_stream"` are string literals pinned by the spec and are allowed — the rule applies to Rust identifiers only.
- Edition 2021; `cargo fmt` clean; `cargo clippy --all-targets -- -D warnings` clean.
- **Builds use the shared cargo target dir:** prefix every cargo command with `CARGO_TARGET_DIR=$HOME/.cargo/shared-target`. Never create per-agent target dirs, never `cargo clean`, debug builds only.
- **Preconditions:** plans 01 and 02 are merged. Plan 01 delivers `pdfboss_core::elements` (`Span`, `Element`, `ElementOpts`, `XrefKind`, `Document::elements(&self, opts: ElementOpts) -> Elements<'_>` with `Iterator<Item = Result<Element>>`), `Document::bytes(&self) -> &[u8]`, and moves `pretty` to `pdfboss_core::pretty` (deleting `crates/pdfboss-cli/src/pretty.rs` and its `mod pretty;` line). Plan 02 delivers `pdfboss-aio` with `AsyncDocument` (`open_url`, `decode_stream`, `read_span`, `file_len(&self) -> u64` — a sync accessor available immediately after open, `elements(&self, opts) -> ElementStream<'_>` implementing `futures_core::Stream<Item = pdfboss_aio::Result<Element>>`) and an `Error` type implementing `Display`. Spec signatures in `docs/superpowers/specs/2026-07-24-pdf-element-explorer-design.md` are the contract; if a signature differs on the ground, the spec wins.
- **Out of scope:** the `tui` subcommand and any dependency on `pdfboss-tui` (plan 05). Do not add either.
- Because `ElementOpts`'s spec text says `Default` yields `physical: true, logical: true`, but derived defaults would be all-false, **always construct `ElementOpts` with every field written out explicitly**; never rely on `Default`.

### Resolved spec ambiguities (pinned here so all tasks agree)

1. **`_objstm` is always present** on object entries: `null` for file-resident objects, `{"_r": [num, gen], "span": [start, end]}` (container ref + byte range within the decoded container stream) for object-stream members. The spec's example shows it as `null` on one object and omits it on another; always-present is the stable choice.
2. **`"length"` inside `"_stream"` is the stored (still encoded) byte count** (`Stream::data.len()`), matching the spec example where it equals `/Length`. `--raw` adds `"data"` = base64 of the stored bytes; `--decode` adds `"data"` = base64 of the decoded bytes (`length` stays the stored count). A failed decode adds `"decode_error": "<message>"` instead of `"data"`.
3. **Content ops** (only with `--content-ops`) appear per page as `"content_ops": [{"op": "<Rust Debug of content::Op>", "_span_in_content": [start, end]}]`. The key is `_span_in_content`, not `_span`, because the range is within the decoded content stream, not the physical file — so `q --hex` never misreads it as a file range.
4. **`startxref`** is the offset carried by the `StartXref` element that sits **physically last in the file** (greatest span start) — the active one. Elements stream in xref-chain order (see rule 13), so "last yielded" is wrong; `build_tree` compares span starts. `null` if none. `Eof` elements do not appear in the tree. Absent header/trailer render as `null`.
5. **`--pages` is 1-based on the command line** (comma separated, e.g. `--pages 1,3`), matching the existing `--page` flag; it converts to 0-based indices for `ElementOpts::pages`. Page 0 is an error.
6. **Element read errors are salvaged:** a failed element prints a `pdfboss: warning: …` line on stderr and is skipped; the tree/dump is built from the rest.
7. **Exit codes:** PDF/IO failures exit 1 (existing convention); invalid jq programs exit 2 (clap already uses 2 for usage errors, and this makes program errors distinguishable from PDF errors as the spec requires). jq **runtime** errors (e.g. `error("boom")`) print `pdfboss: jq: …` and exit 1.
8. **Objects map ordering:** entries are inserted in element (file) order and `serde_json`'s `preserve_order` feature keeps that order, so dumps are deterministic. PDF dictionaries convert with sorted keys (core's `Dict` iteration order is not deterministic).
9. **Whole-file length** (for `hex` with no selector) comes from `AsyncDocument::file_len(&self) -> u64` for URLs (plan 02's sync accessor, available immediately after open) and from `Document::bytes().len()` for local files. No element-derived workaround.
10. **`hex --annotate` boundary lines** are emitted at each physical element's span **start** (`── obj 12 0 ──` style). Object-stream members are skipped (they'd duplicate their container's offset). A mark falling mid-row prints before the row containing it.
11. **`q --hex` on a non-`_span` result** falls back to printing that result as JSON (only objects with a 2-element numeric `_span`, or arrays made entirely of such objects, are hexdumped). Each dumped range is preceded by a `── 0x<start>..0x<end> ──` heading.
12. **Extra deps beyond the spec's list** (`jaq-core`, `jaq-std`, `serde_json`, `base64`, `pdfboss-aio`, tokio): `jaq-json` (the jaq 2.x value type + serde_json bridge — the engine is unusable without it) and `futures-core` (to drive `ElementStream` without pulling in futures-util). Both are CLI-only, consistent with the spec's confinement rule.
13. **Element-stream ordering parity (plans 01/02):** the `Trailer` element is emitted **once** per document (merged trailer dict; span = the newest trailer region), and xref sections stream in **chain order (newest to oldest)**, not ascending file order. Consequences pinned here: the value tree's `xref` array lists sections in chain order as yielded (deterministic); the `xref:N` selector indexes sections in that same chain order (`xref:0` = newest); and `--annotate` must not assume ascending element order — `element_marks` sorts marks by offset before the dump walks the file.

---

### Task 1: Dependencies, exit-code plumbing, and the input abstraction

**Files:**
- Modify: `crates/pdfboss-cli/Cargo.toml` (dependencies section, currently lines 9–13; dev-dependencies added)
- Modify: `crates/pdfboss-cli/src/main.rs` (module decls near line 4; `main()` currently lines 94–113; tests module at end)
- Create: `crates/pdfboss-cli/src/input.rs`
- Test: unit tests inside `crates/pdfboss-cli/src/input.rs` and `crates/pdfboss-cli/src/main.rs`

**Interfaces:**
- Consumes: `pdfboss_core::Document::{load(Vec<u8>) -> Result<Document>, bytes(&self) -> &[u8], stream_data(&self, &Stream) -> Result<Vec<u8>>, elements(&self, ElementOpts) -> Elements<'_>}`; `pdfboss_core::elements::{Element, ElementOpts, Span}`; `pdfboss_aio::AsyncDocument::{open_url, decode_stream, read_span, file_len(&self) -> u64, elements}`; `futures_core::Stream`.
- Produces (later tasks rely on these exactly):
  - `pub struct Failure { pub message: String, pub code: i32 }` with `Failure::new(impl Into<String>) -> Failure` (code 1), `Failure::program(impl Into<String>) -> Failure` (code 2), `impl From<String> for Failure` (in `main.rs`, referenced as `crate::Failure`)
  - `pub fn input::use_color() -> bool`
  - `pub fn input::is_url(spec: &str) -> bool`
  - `pub enum input::Input { Local { doc: Document }, Remote { rt: tokio::runtime::Runtime, doc: AsyncDocument } }` (no byte-buffer copy: `Document::bytes()` serves local reads)
  - `impl Input`: `pub fn open(spec: &str) -> Result<Input, String>`, `pub fn collect_elements(&self, opts: ElementOpts) -> Vec<Element>`, `pub fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>, String>`, `pub fn read_span(&self, span: Span) -> Result<Vec<u8>, String>`, `pub fn file_len(&self) -> u64`

**Steps:**

- [ ] Update `crates/pdfboss-cli/Cargo.toml` to exactly:

```toml
[package]
name = "pdfboss-cli"
description = "Command-line interface for pdfboss: info, text, render and obj subcommands"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
pdfboss-core = { path = "../pdfboss-core" }
pdfboss-text = { path = "../pdfboss-text" }
pdfboss-render = { path = "../pdfboss-render" }
pdfboss-aio = { path = "../pdfboss-aio", features = ["http"] }
clap = { version = "4", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
base64 = "0.22"
tokio = { version = "1", features = ["rt", "net", "time"] }
futures-core = "0.3"

[dev-dependencies]
pdfboss-testkit = { path = "../pdfboss-testkit" }

[features]
# Passthrough so `cargo build -p pdfboss-cli --features substitute-fonts`
# enables the bundled substitute faces, without the caller needing to know
# the non-obvious `--features pdfboss-render/substitute-fonts` spelling.
substitute-fonts = ["pdfboss-render/substitute-fonts"]

[[bin]]
name = "pdfboss"
path = "src/main.rs"
```

(The jaq crates arrive in Task 6 so the jq-engine task is self-contained. `preserve_order` makes `serde_json` maps keep insertion order — resolved ambiguity 8. tokio's `net`/`time` features guarantee `Builder::enable_all()` registers the io and time drivers the HTTP backend needs.)

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo check -p pdfboss-cli` — expect success (deps resolve; `pdfboss-aio` exists per precondition). If `pdfboss-aio` is missing, STOP: plan 02 has not landed.
- [ ] Add failing unit tests for `Failure` to the existing `mod tests` block at the bottom of `crates/pdfboss-cli/src/main.rs` (append inside the module, after `default_out_names_by_page`):

```rust
    #[test]
    fn failure_from_string_exits_one() {
        let failure = Failure::from("boom".to_string());
        assert_eq!(failure.code, 1);
        assert_eq!(failure.message, "boom");
    }

    #[test]
    fn failure_program_exits_two() {
        let failure = Failure::program("bad program");
        assert_eq!(failure.code, 2);
        assert_eq!(failure.message, "bad program");
    }
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli failure_` — expect a **compile error**: `cannot find struct, variant or union type 'Failure'`.
- [ ] Implement `Failure` in `crates/pdfboss-cli/src/main.rs`. Insert immediately after the `use pdfboss_core::…` imports:

```rust
/// A fatal CLI failure: message for stderr plus the process exit code.
/// PDF/IO problems exit 1; invalid jq programs exit 2 (mirroring clap's own
/// usage-error code and keeping the two failure kinds distinguishable).
pub struct Failure {
    pub message: String,
    pub code: i32,
}

impl Failure {
    /// A PDF/IO failure (exit code 1).
    pub fn new(message: impl Into<String>) -> Failure {
        Failure {
            message: message.into(),
            code: 1,
        }
    }

    /// An invalid-program failure (exit code 2).
    pub fn program(message: impl Into<String>) -> Failure {
        Failure {
            message: message.into(),
            code: 2,
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Failure {
        Failure::new(message)
    }
}
```

and replace the whole `fn main()` (currently lines 94–113) with:

```rust
fn main() {
    let cli = Cli::parse();
    let result: Result<(), Failure> = match cli.command {
        Command::Info { file } => cmd_info(&file).map_err(Failure::from),
        Command::Text { file, page } => cmd_text(&file, page).map_err(Failure::from),
        Command::Render {
            file,
            page,
            out,
            scale,
            fonts,
            font_dir,
        } => cmd_render(&file, page, out, scale, fonts, font_dir).map_err(Failure::from),
        Command::Obj { file, num, gen } => {
            cmd_obj(&file, num, gen.unwrap_or(0)).map_err(Failure::from)
        }
    };
    if let Err(failure) = result {
        eprintln!("pdfboss: {}", failure.message);
        std::process::exit(failure.code);
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli failure_` — expect both tests pass.
- [ ] Add `mod input;` to `crates/pdfboss-cli/src/main.rs`, directly below the crate doc comment (where `mod pretty;` used to sit before plan 01 removed it), and create `crates/pdfboss-cli/src/input.rs` containing the full module **including its failing tests**:

```rust
//! Shared input handling for the explorer subcommands (`json`, `hex`, `q`):
//! local files through the sync `Document` fast path, `http(s)` URLs through
//! the async HTTP backend on a small single-thread runtime.

use std::io::IsTerminal as _;

use futures_core::Stream as _;
use pdfboss_aio::AsyncDocument;
use pdfboss_core::elements::{Element, ElementOpts, Span};
use pdfboss_core::{Document, Stream};

/// Whether stdout should carry ANSI colors: only on a tty, and never when
/// `NO_COLOR` is set (any value, per the NO_COLOR convention).
pub fn use_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// True for inputs fetched over HTTP; everything else is a local path.
pub fn is_url(spec: &str) -> bool {
    spec.starts_with("http://") || spec.starts_with("https://")
}

/// One opened PDF input, local or remote.
pub enum Input {
    Local {
        doc: Document,
    },
    Remote {
        rt: tokio::runtime::Runtime,
        doc: AsyncDocument,
    },
}

impl Input {
    /// Opens `spec`: an `http(s)://` URL via the aio HTTP backend, anything
    /// else as a local file via the sync fast path.
    pub fn open(spec: &str) -> Result<Input, String> {
        if is_url(spec) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            let doc = rt
                .block_on(AsyncDocument::open_url(spec))
                .map_err(|e| e.to_string())?;
            Ok(Input::Remote { rt, doc })
        } else {
            let bytes = std::fs::read(spec).map_err(|e| format!("{spec}: {e}"))?;
            let doc = Document::load(bytes).map_err(|e| e.to_string())?;
            Ok(Input::Local { doc })
        }
    }

    /// Collects the document's elements. Unreadable elements are skipped with
    /// a warning on stderr (salvage semantics: one bad object must not kill
    /// exploration).
    pub fn collect_elements(&self, opts: ElementOpts) -> Vec<Element> {
        match self {
            Input::Local { doc, .. } => doc
                .elements(opts)
                .filter_map(|item| match item {
                    Ok(element) => Some(element),
                    Err(e) => {
                        eprintln!("pdfboss: warning: skipping unreadable element: {e}");
                        None
                    }
                })
                .collect(),
            Input::Remote { rt, doc } => rt.block_on(async {
                let mut out = Vec::new();
                let mut stream = std::pin::pin!(doc.elements(opts));
                while let Some(item) =
                    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
                {
                    match item {
                        Ok(element) => out.push(element),
                        Err(e) => {
                            eprintln!("pdfboss: warning: skipping unreadable element: {e}");
                        }
                    }
                }
                out
            }),
        }
    }

    /// Decodes a stream's data through its filter chain.
    pub fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>, String> {
        match self {
            Input::Local { doc, .. } => doc.stream_data(s).map_err(|e| e.to_string()),
            Input::Remote { rt, doc } => {
                rt.block_on(doc.decode_stream(s)).map_err(|e| e.to_string())
            }
        }
    }

    /// Raw bytes for `span` (end-exclusive), for hex views.
    pub fn read_span(&self, span: Span) -> Result<Vec<u8>, String> {
        match self {
            Input::Local { doc } => {
                let bytes = doc.bytes();
                let start = usize::try_from(span.start).ok();
                let end = usize::try_from(span.end).ok();
                match (start, end) {
                    (Some(start), Some(end)) if start <= end && end <= bytes.len() => {
                        Ok(bytes[start..end].to_vec())
                    }
                    _ => Err(format!(
                        "span {}..{} lies outside the file ({} bytes)",
                        span.start,
                        span.end,
                        bytes.len()
                    )),
                }
            }
            Input::Remote { rt, doc } => {
                rt.block_on(doc.read_span(span)).map_err(|e| e.to_string())
            }
        }
    }

    /// Total length of the underlying file in bytes.
    pub fn file_len(&self) -> u64 {
        match self {
            Input::Local { doc } => doc.bytes().len() as u64,
            Input::Remote { doc, .. } => doc.file_len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        format!("{}/../../tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)
    }

    fn physical_opts() -> ElementOpts {
        ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        }
    }

    #[test]
    fn url_detection() {
        assert!(is_url("http://example.com/a.pdf"));
        assert!(is_url("https://example.com/a.pdf"));
        assert!(!is_url("a.pdf"));
        assert!(!is_url("/tmp/a.pdf"));
        assert!(!is_url("httpx://nope"));
    }

    #[test]
    fn local_open_collects_header_first() {
        let input = Input::open(&fixture("hello.pdf")).expect("fixture opens");
        let elements = input.collect_elements(physical_opts());
        assert!(!elements.is_empty(), "no elements from hello.pdf");
        assert!(
            matches!(elements[0], Element::Header { .. }),
            "first physical element must be the header"
        );
    }

    #[test]
    fn local_read_span_and_file_len() {
        let input = Input::open(&fixture("hello.pdf")).expect("fixture opens");
        let len = input.file_len();
        assert!(len > 0);
        let head = input
            .read_span(Span { start: 0, end: 8 })
            .expect("in bounds");
        assert_eq!(&head, b"%PDF-1.7");
        assert!(input
            .read_span(Span {
                start: 0,
                end: len + 1
            })
            .is_err());
        assert!(input.read_span(Span { start: 9, end: 8 }).is_err());
    }

    #[test]
    fn missing_local_file_reports_path() {
        let err = Input::open("definitely-not-here.pdf").expect_err("missing file");
        assert!(
            err.contains("definitely-not-here.pdf"),
            "path missing from: {err}"
        );
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli input::` — expect all 4 `input::tests` tests pass (the module and its tests land together; the earlier `failure_` cycle already exercised red-green for this task's `main.rs` half). If `Document::elements`, `Document::bytes`, or the `elements` module is missing, STOP: plan 01 has not landed.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli` — expect the full existing suite (`cli` integration tests included) still green.
- [ ] Run `cargo fmt -p pdfboss-cli` then commit:

```bash
git add crates/pdfboss-cli/Cargo.toml crates/pdfboss-cli/src/main.rs crates/pdfboss-cli/src/input.rs
git commit -m "feat(cli): input abstraction and exit-code plumbing for explorer subcommands"
```

### Task 2: Value tree conversion (`src/q/value.rs`)

**Files:**
- Create: `crates/pdfboss-cli/src/q/mod.rs`, `crates/pdfboss-cli/src/q/value.rs`
- Modify: `crates/pdfboss-cli/src/main.rs` (add `mod q;` beside `mod input;`)
- Test: unit tests inside `crates/pdfboss-cli/src/q/value.rs`

**Interfaces:**
- Consumes: `pdfboss_core::elements::{Element, ElementOpts, Span, XrefKind}` (spec); `pdfboss_core::{Dict, Name, ObjRef, Object, Stream}` (`Name(pub String)`, `Dict::iter()`, `Stream { dict, data }`, `Object` variants `Null/Bool/Int/Real/String/Name/Array/Dict/Stream/Ref` from `crates/pdfboss-core/src/object.rs`); `pdfboss_core::content::Op` (Debug).
- Produces (later tasks rely on these exactly):
  - `pub enum StreamData { Omit, Raw, Decode }`
  - `pub struct TreeFlags { pub raw: bool, pub decode: bool, pub pages: Option<Vec<usize>>, pub no_logical: bool, pub content_ops: bool }` with `pub fn stream_data(&self) -> StreamData` and `pub fn element_opts(&self) -> Result<ElementOpts, String>`
  - `pub fn object_to_value(obj: &Object, mode: StreamData, decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>) -> serde_json::Value`
  - `pub fn build_tree(elements: &[Element], mode: StreamData, include_content_ops: bool, decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>) -> serde_json::Value`

**Steps:**

- [ ] Add `mod q;` to `crates/pdfboss-cli/src/main.rs` (directly after `mod input;`), create `crates/pdfboss-cli/src/q/mod.rs`:

```rust
//! `pdfboss q`: document to JSON value tree, and the jq engine that queries it.

pub mod value;
```

and create `crates/pdfboss-cli/src/q/value.rs` with **only** the doc comment, imports, and the failing test module for `object_to_value` (implementation comes after the red run):

```rust
//! Conversion of a document's elements into a `serde_json::Value` tree — the
//! input to `pdfboss q` and `pdfboss json`. Wire format (pinned by the spec):
//! metadata keys are underscore-prefixed JSON keys (`_span`, `_ref`, `_kind`,
//! `_objstm`); indirect references are `{"_r": [num, gen]}`; names are plain
//! strings; PDF strings are UTF-8 where valid else `{"_bytes": "<base64>"}`;
//! streams are `{"_stream": {"dict": …, "length": N}}` with data embedded
//! only under `--raw` / `--decode`.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use pdfboss_core::elements::{Element, ElementOpts, Span, XrefKind};
use pdfboss_core::{Dict, Object, Stream};
use serde_json::{json, Map, Value};

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{Name, ObjRef};

    fn no_decode() -> impl FnMut(&Stream) -> Result<Vec<u8>, String> {
        |s: &Stream| {
            let _ = s;
            Err("decode must not be called".to_string())
        }
    }

    fn plain_stream() -> Stream {
        let mut dict = Dict::new();
        dict.insert(Name("Length".to_string()), Object::Int(2));
        Stream {
            dict,
            data: b"hi".to_vec(),
        }
    }

    #[test]
    fn scalars_convert_directly() {
        let mut decode = no_decode();
        assert_eq!(
            object_to_value(&Object::Null, StreamData::Omit, &mut decode),
            Value::Null
        );
        assert_eq!(
            object_to_value(&Object::Bool(true), StreamData::Omit, &mut decode),
            json!(true)
        );
        assert_eq!(
            object_to_value(&Object::Int(-42), StreamData::Omit, &mut decode),
            json!(-42)
        );
        assert_eq!(
            object_to_value(&Object::Real(1.5), StreamData::Omit, &mut decode),
            json!(1.5)
        );
        assert_eq!(
            object_to_value(
                &Object::Name(Name("Page".to_string())),
                StreamData::Omit,
                &mut decode
            ),
            json!("Page")
        );
        assert_eq!(
            object_to_value(
                &Object::Ref(ObjRef { num: 13, gen: 0 }),
                StreamData::Omit,
                &mut decode
            ),
            json!({ "_r": [13, 0] })
        );
    }

    #[test]
    fn nan_real_becomes_null() {
        let mut decode = no_decode();
        assert_eq!(
            object_to_value(&Object::Real(f64::NAN), StreamData::Omit, &mut decode),
            Value::Null
        );
    }

    #[test]
    fn strings_are_utf8_or_base64_bytes() {
        let mut decode = no_decode();
        assert_eq!(
            object_to_value(
                &Object::String(b"hello".to_vec()),
                StreamData::Omit,
                &mut decode
            ),
            json!("hello")
        );
        assert_eq!(
            object_to_value(
                &Object::String(vec![0xff, 0xfe]),
                StreamData::Omit,
                &mut decode
            ),
            json!({ "_bytes": "//4=" })
        );
    }

    #[test]
    fn dict_keys_are_sorted_names() {
        let mut dict = Dict::new();
        dict.insert(Name("B".to_string()), Object::Int(2));
        dict.insert(Name("A".to_string()), Object::Int(1));
        let mut decode = no_decode();
        let v = object_to_value(&Object::Dict(dict), StreamData::Omit, &mut decode);
        // preserve_order keeps insertion order, so serialization proves the
        // conversion inserted keys sorted.
        assert_eq!(serde_json::to_string(&v).expect("serializes"), r#"{"A":1,"B":2}"#);
    }

    #[test]
    fn arrays_convert_recursively() {
        let mut decode = no_decode();
        let v = object_to_value(
            &Object::Array(vec![Object::Int(1), Object::Array(vec![Object::Int(2)])]),
            StreamData::Omit,
            &mut decode,
        );
        assert_eq!(v, json!([1, [2]]));
    }

    #[test]
    fn stream_data_is_omitted_by_default() {
        let mut decode = no_decode();
        let v = object_to_value(
            &Object::Stream(plain_stream()),
            StreamData::Omit,
            &mut decode,
        );
        assert_eq!(v, json!({ "_stream": { "dict": { "Length": 2 }, "length": 2 } }));
    }

    #[test]
    fn raw_mode_embeds_raw_base64() {
        let mut decode = no_decode();
        let v = object_to_value(
            &Object::Stream(plain_stream()),
            StreamData::Raw,
            &mut decode,
        );
        assert_eq!(
            v,
            json!({ "_stream": { "dict": { "Length": 2 }, "length": 2, "data": "aGk=" } })
        );
    }

    #[test]
    fn decode_mode_embeds_decoded_base64() {
        let mut decode = |s: &Stream| {
            let _ = s;
            Ok(b"HI".to_vec())
        };
        let v = object_to_value(
            &Object::Stream(plain_stream()),
            StreamData::Decode,
            &mut decode,
        );
        assert_eq!(
            v,
            json!({ "_stream": { "dict": { "Length": 2 }, "length": 2, "data": "SEk=" } })
        );
    }

    #[test]
    fn decode_failure_is_reported_inline() {
        let mut decode = |s: &Stream| {
            let _ = s;
            Err("kaput".to_string())
        };
        let v = object_to_value(
            &Object::Stream(plain_stream()),
            StreamData::Decode,
            &mut decode,
        );
        assert_eq!(
            v,
            json!({ "_stream": { "dict": { "Length": 2 }, "length": 2, "decode_error": "kaput" } })
        );
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli q::value::` — expect a **compile error**: `cannot find function 'object_to_value'` / `cannot find type 'StreamData'`.
- [ ] Implement the conversion in `crates/pdfboss-cli/src/q/value.rs`, inserted between the imports and the test module:

```rust
/// How stream data is embedded in the value tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamData {
    /// Data omitted; only the dict and the stored byte count appear (default).
    Omit,
    /// `data` carries the raw (still encoded) bytes, base64.
    Raw,
    /// `data` carries the decoded bytes, base64.
    Decode,
}

/// Flags shared by `json` and `q` that shape the value tree.
pub struct TreeFlags {
    pub raw: bool,
    pub decode: bool,
    pub pages: Option<Vec<usize>>,
    pub no_logical: bool,
    pub content_ops: bool,
}

impl TreeFlags {
    /// `--decode` wins over `--raw` (clap marks them conflicting anyway).
    pub fn stream_data(&self) -> StreamData {
        if self.decode {
            StreamData::Decode
        } else if self.raw {
            StreamData::Raw
        } else {
            StreamData::Omit
        }
    }

    /// Maps the CLI flags onto core's `ElementOpts`. `--pages` is 1-based on
    /// the command line (matching `--page` elsewhere) and 0-based in core.
    pub fn element_opts(&self) -> Result<ElementOpts, String> {
        let pages = match &self.pages {
            None => None,
            Some(numbers) => {
                let mut indices = Vec::with_capacity(numbers.len());
                for &n in numbers {
                    if n == 0 {
                        return Err("--pages is 1-based; page 0 does not exist".to_string());
                    }
                    indices.push(n - 1);
                }
                Some(indices)
            }
        };
        Ok(ElementOpts {
            physical: true,
            logical: !self.no_logical,
            pages,
            content_ops: self.content_ops,
        })
    }
}

/// `[start, end]` per the wire format.
fn span_value(span: Span) -> Value {
    json!([span.start, span.end])
}

/// Converts one PDF object to JSON per the wire format.
pub fn object_to_value(
    obj: &Object,
    mode: StreamData,
    decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>,
) -> Value {
    match obj {
        Object::Null => Value::Null,
        Object::Bool(b) => Value::Bool(*b),
        Object::Int(i) => Value::from(*i),
        Object::Real(r) => serde_json::Number::from_f64(*r)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Object::String(bytes) => string_to_value(bytes),
        Object::Name(name) => Value::String(name.0.clone()),
        Object::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| object_to_value(item, mode, decode))
                .collect(),
        ),
        Object::Dict(dict) => dict_to_value(dict, mode, decode),
        Object::Stream(s) => stream_to_value(s, mode, decode),
        Object::Ref(r) => json!({ "_r": [r.num, r.gen] }),
    }
}

fn string_to_value(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => Value::String(text.to_string()),
        Err(_) => json!({ "_bytes": BASE64.encode(bytes) }),
    }
}

/// Dictionary entries sorted by name: core's `Dict` iteration order is not
/// deterministic, and dumps must be byte-stable across runs.
fn dict_to_value(
    dict: &Dict,
    mode: StreamData,
    decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>,
) -> Value {
    let mut entries: Vec<_> = dict.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut map = Map::new();
    for (name, value) in entries {
        map.insert(name.0.clone(), object_to_value(value, mode, decode));
    }
    Value::Object(map)
}

fn stream_to_value(
    s: &Stream,
    mode: StreamData,
    decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>,
) -> Value {
    let mut inner = Map::new();
    inner.insert("dict".to_string(), dict_to_value(&s.dict, mode, decode));
    inner.insert("length".to_string(), Value::from(s.data.len() as u64));
    match mode {
        StreamData::Omit => {}
        StreamData::Raw => {
            inner.insert("data".to_string(), Value::String(BASE64.encode(&s.data)));
        }
        StreamData::Decode => match decode(s) {
            Ok(data) => {
                inner.insert("data".to_string(), Value::String(BASE64.encode(&data)));
            }
            Err(message) => {
                inner.insert("decode_error".to_string(), Value::String(message));
            }
        },
    }
    let mut outer = Map::new();
    outer.insert("_stream".to_string(), Value::Object(inner));
    Value::Object(outer)
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli q::value::` — expect all 9 tests pass.
- [ ] Append the failing `build_tree` tests inside the existing `mod tests` block in `crates/pdfboss-cli/src/q/value.rs`:

```rust
    #[test]
    fn build_tree_matches_the_wire_format() {
        let mut dict = Dict::new();
        dict.insert(
            Name("Type".to_string()),
            Object::Name(Name("Page".to_string())),
        );
        dict.insert(
            Name("Contents".to_string()),
            Object::Ref(ObjRef { num: 13, gen: 0 }),
        );
        let mut trailer_dict = Dict::new();
        trailer_dict.insert(
            Name("Root".to_string()),
            Object::Ref(ObjRef { num: 1, gen: 0 }),
        );
        trailer_dict.insert(Name("Size".to_string()), Object::Int(42));
        let elements = vec![
            Element::Header {
                version: (1, 7),
                span: Span { start: 0, end: 15 },
            },
            Element::IndirectObject {
                r: ObjRef { num: 12, gen: 0 },
                object: Object::Dict(dict),
                span: Span {
                    start: 6720,
                    end: 6914,
                },
                in_objstm: None,
            },
            Element::XrefSection {
                kind: XrefKind::Table,
                span: Span {
                    start: 7480,
                    end: 8322,
                },
                entries: 42,
            },
            Element::Trailer {
                dict: trailer_dict,
                span: Span {
                    start: 8322,
                    end: 8419,
                },
            },
            Element::StartXref {
                offset: 7480,
                span: Span {
                    start: 8419,
                    end: 8434,
                },
            },
            Element::Eof {
                span: Span {
                    start: 8434,
                    end: 8440,
                },
            },
        ];
        let mut decode = no_decode();
        let tree = build_tree(&elements, StreamData::Omit, false, &mut decode);
        assert_eq!(
            tree,
            json!({
                "header": { "version": "1.7", "_span": [0, 15], "_kind": "header" },
                "objects": {
                    "12 0": {
                        "_kind": "object",
                        "_ref": [12, 0],
                        "_span": [6720, 6914],
                        "_objstm": null,
                        "value": { "Contents": {"_r": [13, 0]}, "Type": "Page" }
                    }
                },
                "pages": [],
                "xref": [ { "kind": "table", "entries": 42, "_span": [7480, 8322] } ],
                "trailer": { "_span": [8322, 8419], "value": { "Root": {"_r": [1, 0]}, "Size": 42 } },
                "startxref": 7480
            })
        );
    }

    #[test]
    fn missing_header_and_trailer_render_as_null() {
        let mut decode = no_decode();
        let tree = build_tree(&[], StreamData::Omit, false, &mut decode);
        assert_eq!(tree["header"], Value::Null);
        assert_eq!(tree["trailer"], Value::Null);
        assert_eq!(tree["startxref"], Value::Null);
        assert_eq!(tree["objects"], json!({}));
        assert_eq!(tree["pages"], json!([]));
        assert_eq!(tree["xref"], json!([]));
    }

    #[test]
    fn startxref_uses_the_physically_last_element_regardless_of_yield_order() {
        // Chain order yields the newest region first; the active startxref is
        // the one at the greatest file offset either way.
        let newest_first = vec![
            Element::StartXref {
                offset: 500,
                span: Span {
                    start: 900,
                    end: 915,
                },
            },
            Element::StartXref {
                offset: 100,
                span: Span {
                    start: 300,
                    end: 315,
                },
            },
        ];
        let mut decode = no_decode();
        let tree = build_tree(&newest_first, StreamData::Omit, false, &mut decode);
        assert_eq!(tree["startxref"], json!(500));

        let oldest_first: Vec<Element> = newest_first.into_iter().rev().collect();
        let tree = build_tree(&oldest_first, StreamData::Omit, false, &mut decode);
        assert_eq!(tree["startxref"], json!(500));
    }

    #[test]
    fn objstm_members_carry_container_and_inner_span() {
        let elements = vec![Element::IndirectObject {
            r: ObjRef { num: 1, gen: 0 },
            object: Object::Bool(true),
            span: Span {
                start: 100,
                end: 400,
            },
            in_objstm: Some((ObjRef { num: 6, gen: 0 }, Span { start: 20, end: 54 })),
        }];
        let mut decode = no_decode();
        let tree = build_tree(&elements, StreamData::Omit, false, &mut decode);
        assert_eq!(
            tree["objects"]["1 0"]["_objstm"],
            json!({ "_r": [6, 0], "span": [20, 54] })
        );
    }

    #[test]
    fn logical_elements_group_under_their_page() {
        let elements = vec![
            Element::Page {
                index: 0,
                r: ObjRef { num: 3, gen: 0 },
            },
            Element::Font {
                page: Some(0),
                r: ObjRef { num: 5, gen: 0 },
                subtype: Name("Type1".to_string()),
                base_font: Some(Name("Helvetica".to_string())),
            },
            Element::Image {
                page: Some(0),
                r: ObjRef { num: 7, gen: 0 },
                width: 100,
                height: 50,
            },
            Element::Annotation {
                page: 0,
                r: ObjRef { num: 9, gen: 0 },
                subtype: Name("Link".to_string()),
            },
            Element::ContentOp {
                page: 0,
                op: pdfboss_core::content::Op::Fill,
                span_in_content: Span { start: 4, end: 6 },
            },
        ];
        let mut decode = no_decode();
        let tree = build_tree(&elements, StreamData::Omit, true, &mut decode);
        assert_eq!(
            tree["pages"],
            json!([{
                "index": 0,
                "_ref": [3, 0],
                "fonts": [ { "_ref": [5, 0], "subtype": "Type1", "base_font": "Helvetica" } ],
                "images": [ { "_ref": [7, 0], "width": 100, "height": 50 } ],
                "annotations": [ { "_ref": [9, 0], "subtype": "Link" } ],
                "content_ops": [ { "op": "Fill", "_span_in_content": [4, 6] } ]
            }])
        );

        let without_ops = build_tree(&elements, StreamData::Omit, false, &mut decode);
        assert!(without_ops["pages"][0].get("content_ops").is_none());
        assert_eq!(without_ops["pages"][0]["fonts"], tree["pages"][0]["fonts"]);
    }

    #[test]
    fn document_level_fonts_without_a_page_are_skipped() {
        let elements = vec![Element::Font {
            page: None,
            r: ObjRef { num: 5, gen: 0 },
            subtype: Name("Type1".to_string()),
            base_font: None,
        }];
        let mut decode = no_decode();
        let tree = build_tree(&elements, StreamData::Omit, false, &mut decode);
        assert_eq!(tree["pages"], json!([]));
    }

    #[test]
    fn tree_flags_map_to_element_opts() {
        let flags = TreeFlags {
            raw: false,
            decode: false,
            pages: Some(vec![1, 3]),
            no_logical: false,
            content_ops: true,
        };
        let opts = flags.element_opts().expect("valid pages");
        assert!(opts.physical);
        assert!(opts.logical);
        assert_eq!(opts.pages, Some(vec![0, 2]));
        assert!(opts.content_ops);

        let no_logical = TreeFlags {
            raw: false,
            decode: false,
            pages: None,
            no_logical: true,
            content_ops: false,
        };
        assert!(!no_logical.element_opts().expect("valid").logical);

        let zero = TreeFlags {
            raw: false,
            decode: false,
            pages: Some(vec![0]),
            no_logical: false,
            content_ops: false,
        };
        assert!(zero.element_opts().is_err());
    }

    #[test]
    fn stream_data_mode_precedence() {
        let base = |raw, decode| TreeFlags {
            raw,
            decode,
            pages: None,
            no_logical: false,
            content_ops: false,
        };
        assert_eq!(base(false, false).stream_data(), StreamData::Omit);
        assert_eq!(base(true, false).stream_data(), StreamData::Raw);
        assert_eq!(base(false, true).stream_data(), StreamData::Decode);
    }
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli q::value::` — expect a **compile error**: `cannot find function 'build_tree'`.
- [ ] Implement `build_tree` in `crates/pdfboss-cli/src/q/value.rs`, appended after `stream_to_value` (before the test module):

```rust
/// Per-page accumulator while walking the logical elements.
#[derive(Default)]
struct PageAcc {
    r: Option<Value>,
    fonts: Vec<Value>,
    images: Vec<Value>,
    annotations: Vec<Value>,
    content_ops: Vec<Value>,
}

/// Builds the full value tree: top-level `header`, `objects` (map keyed
/// `"N G"`), `pages`, `xref`, `trailer`, `startxref`. `include_content_ops`
/// controls whether page entries carry a `content_ops` array (the elements
/// only contain ops when `ElementOpts::content_ops` was set).
pub fn build_tree(
    elements: &[Element],
    mode: StreamData,
    include_content_ops: bool,
    decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>,
) -> Value {
    let mut header = Value::Null;
    let mut objects = Map::new();
    let mut xref: Vec<Value> = Vec::new();
    let mut trailer = Value::Null;
    let mut startxref = Value::Null;
    // Elements stream xref sections in chain order (newest first), so the
    // active startxref is the one physically last in the file, not the last
    // one yielded.
    let mut startxref_pos: Option<u64> = None;
    let mut page_acc: std::collections::BTreeMap<usize, PageAcc> = std::collections::BTreeMap::new();

    for element in elements {
        match element {
            Element::Header { version, span } => {
                header = json!({
                    "version": format!("{}.{}", version.0, version.1),
                    "_span": span_value(*span),
                    "_kind": "header",
                });
            }
            Element::IndirectObject {
                r,
                object,
                span,
                in_objstm,
            } => {
                let objstm = match in_objstm {
                    None => Value::Null,
                    Some((container, inner)) => json!({
                        "_r": [container.num, container.gen],
                        "span": span_value(*inner),
                    }),
                };
                let entry = json!({
                    "_kind": "object",
                    "_ref": [r.num, r.gen],
                    "_span": span_value(*span),
                    "_objstm": objstm,
                    "value": object_to_value(object, mode, decode),
                });
                objects.insert(format!("{} {}", r.num, r.gen), entry);
            }
            Element::XrefSection {
                kind,
                span,
                entries,
            } => {
                let kind = match kind {
                    XrefKind::Table => "table",
                    XrefKind::Stream => "stream",
                };
                xref.push(json!({
                    "kind": kind,
                    "entries": *entries,
                    "_span": span_value(*span),
                }));
            }
            Element::Trailer { dict, span } => {
                // Emitted once per document (merged trailer dict; span is the
                // newest trailer region).
                trailer = json!({
                    "_span": span_value(*span),
                    "value": dict_to_value(dict, mode, decode),
                });
            }
            Element::StartXref { offset, span } => {
                if startxref_pos.is_none_or(|pos| span.start >= pos) {
                    startxref_pos = Some(span.start);
                    startxref = Value::from(*offset);
                }
            }
            Element::Eof { .. } => {}
            Element::Page { index, r } => {
                let acc = page_acc.entry(*index).or_default();
                acc.r = Some(json!([r.num, r.gen]));
            }
            Element::Font {
                page,
                r,
                subtype,
                base_font,
            } => {
                if let Some(page) = page {
                    page_acc.entry(*page).or_default().fonts.push(json!({
                        "_ref": [r.num, r.gen],
                        "subtype": subtype.0.clone(),
                        "base_font": base_font.as_ref().map(|n| n.0.clone()),
                    }));
                }
            }
            Element::Image {
                page,
                r,
                width,
                height,
            } => {
                if let Some(page) = page {
                    page_acc.entry(*page).or_default().images.push(json!({
                        "_ref": [r.num, r.gen],
                        "width": *width,
                        "height": *height,
                    }));
                }
            }
            Element::Annotation { page, r, subtype } => {
                page_acc.entry(*page).or_default().annotations.push(json!({
                    "_ref": [r.num, r.gen],
                    "subtype": subtype.0.clone(),
                }));
            }
            Element::ContentOp {
                page,
                op,
                span_in_content,
            } => {
                page_acc.entry(*page).or_default().content_ops.push(json!({
                    "op": format!("{op:?}"),
                    "_span_in_content": span_value(*span_in_content),
                }));
            }
        }
    }

    let pages: Vec<Value> = page_acc
        .into_iter()
        .map(|(index, acc)| {
            let mut page = Map::new();
            page.insert("index".to_string(), Value::from(index as u64));
            page.insert("_ref".to_string(), acc.r.unwrap_or(Value::Null));
            page.insert("fonts".to_string(), Value::Array(acc.fonts));
            page.insert("images".to_string(), Value::Array(acc.images));
            page.insert("annotations".to_string(), Value::Array(acc.annotations));
            if include_content_ops {
                page.insert("content_ops".to_string(), Value::Array(acc.content_ops));
            }
            Value::Object(page)
        })
        .collect();

    let mut root = Map::new();
    root.insert("header".to_string(), header);
    root.insert("objects".to_string(), Value::Object(objects));
    root.insert("pages".to_string(), Value::Array(pages));
    root.insert("xref".to_string(), Value::Array(xref));
    root.insert("trailer".to_string(), trailer);
    root.insert("startxref".to_string(), startxref);
    Value::Object(root)
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli q::value::` — expect all 17 tests pass.
- [ ] Run `cargo fmt -p pdfboss-cli` and `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-cli --all-targets -- -D warnings` — fix any lint, then commit:

```bash
git add crates/pdfboss-cli/src/main.rs crates/pdfboss-cli/src/q
git commit -m "feat(cli): convert documents to the fq-style JSON value tree"
```

### Task 3: JSON pretty printer and the `pdfboss json` subcommand

**Files:**
- Create: `crates/pdfboss-cli/src/json.rs`
- Modify: `crates/pdfboss-cli/src/main.rs` (module decls; `Command` enum, currently lines 23–69; `main()`)
- Test: unit tests inside `crates/pdfboss-cli/src/json.rs` (integration goldens land in Task 8)

**Interfaces:**
- Consumes: Task 1's `Input`/`use_color`, Task 2's `TreeFlags`/`build_tree`; `pdfboss_core::Stream`.
- Produces (later tasks rely on these exactly):
  - `pub fn json::write_json_pretty(out: &mut String, v: &serde_json::Value, indent: usize, color: bool)`
  - `pub fn json::cmd_json(input_spec: &str, flags: &TreeFlags) -> Result<(), String>`
  - `Command::Json { input: String, raw: bool, decode: bool, pages: Option<Vec<usize>>, no_logical: bool, content_ops: bool }` wired in `main()`

**Steps:**

- [ ] Add `mod json;` to `crates/pdfboss-cli/src/main.rs` (after `mod input;`) and create `crates/pdfboss-cli/src/json.rs` with the doc comment, imports, and failing printer tests:

```rust
//! `pdfboss json`: dump the whole document as a pretty-printed JSON value
//! tree, plus the JSON writer shared with `pdfboss q`.

use pdfboss_core::Stream;
use serde_json::Value;

use crate::input::{use_color, Input};
use crate::q::value::{build_tree, TreeFlags};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plain(v: &Value) -> String {
        let mut out = String::new();
        write_json_pretty(&mut out, v, 0, false);
        out
    }

    #[test]
    fn scalars_print_bare() {
        assert_eq!(plain(&json!(null)), "null");
        assert_eq!(plain(&json!(true)), "true");
        assert_eq!(plain(&json!(42)), "42");
        assert_eq!(plain(&json!(1.5)), "1.5");
        assert_eq!(plain(&json!("a\"b")), r#""a\"b""#);
    }

    #[test]
    fn empty_containers_stay_inline() {
        assert_eq!(plain(&json!([])), "[]");
        assert_eq!(plain(&json!({})), "{}");
    }

    #[test]
    fn pretty_prints_nested_containers_two_space_indented() {
        let v = json!({"a": [1, 2], "b": {"c": "x"}});
        assert_eq!(
            plain(&v),
            "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {\n    \"c\": \"x\"\n  }\n}"
        );
    }

    #[test]
    fn color_paints_keys_strings_numbers_and_literals() {
        let v = json!({"k": ["s", 7, true, null]});
        let mut out = String::new();
        write_json_pretty(&mut out, &v, 0, true);
        assert!(out.contains("\x1b[36m\"k\"\x1b[0m"), "key not cyan: {out:?}");
        assert!(out.contains("\x1b[32m\"s\"\x1b[0m"), "string not green: {out:?}");
        assert!(out.contains("\x1b[33m7\x1b[0m"), "number not yellow: {out:?}");
        assert!(out.contains("\x1b[35mtrue\x1b[0m"), "bool not magenta: {out:?}");
        assert!(out.contains("\x1b[35mnull\x1b[0m"), "null not magenta: {out:?}");
    }

    #[test]
    fn uncolored_output_carries_no_escapes() {
        let v = json!({"k": [1, "s"]});
        assert!(!plain(&v).contains('\x1b'));
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli json::` — expect a **compile error**: `cannot find function 'write_json_pretty'`.
- [ ] Implement the printer and the subcommand in `crates/pdfboss-cli/src/json.rs`, inserted between the imports and the test module:

```rust
/// `pdfboss json <file-or-url> [--raw|--decode] [--pages ..] [--no-logical]
/// [--content-ops]`: dumps the full value tree for piping to external tools.
pub fn cmd_json(input_spec: &str, flags: &TreeFlags) -> Result<(), String> {
    let input = Input::open(input_spec)?;
    let opts = flags.element_opts()?;
    let elements = input.collect_elements(opts);
    let mut decode = |s: &Stream| input.decode_stream(s);
    let tree = build_tree(&elements, flags.stream_data(), flags.content_ops, &mut decode);
    let mut text = String::new();
    write_json_pretty(&mut text, &tree, 0, use_color());
    println!("{text}");
    Ok(())
}

/// Two-space-indented JSON with optional ANSI coloring: keys cyan, strings
/// green, numbers yellow, booleans/null magenta. The uncolored layout is the
/// stable wire format golden tests pin.
pub fn write_json_pretty(out: &mut String, v: &Value, indent: usize, color: bool) {
    const KEY: &str = "\x1b[36m";
    const STR: &str = "\x1b[32m";
    const NUM: &str = "\x1b[33m";
    const LIT: &str = "\x1b[35m";
    const RESET: &str = "\x1b[0m";
    let paint = |out: &mut String, code: &str, text: &str| {
        if color {
            out.push_str(code);
            out.push_str(text);
            out.push_str(RESET);
        } else {
            out.push_str(text);
        }
    };
    match v {
        Value::Null => paint(out, LIT, "null"),
        Value::Bool(b) => paint(out, LIT, if *b { "true" } else { "false" }),
        Value::Number(n) => paint(out, NUM, &n.to_string()),
        Value::String(s) => {
            let quoted = serde_json::to_string(s).expect("strings always serialize");
            paint(out, STR, &quoted);
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                push_indent(out, indent + 1);
                write_json_pretty(out, item, indent + 1, color);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (key, value)) in map.iter().enumerate() {
                push_indent(out, indent + 1);
                let quoted = serde_json::to_string(key).expect("strings always serialize");
                paint(out, KEY, &quoted);
                out.push_str(": ");
                write_json_pretty(out, value, indent + 1, color);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent);
            out.push('}');
        }
    }
}

fn push_indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli json::` — expect all 5 tests pass.
- [ ] Wire the subcommand. In `crates/pdfboss-cli/src/main.rs`, append this variant to the `Command` enum (after the `Obj` variant, before the closing brace):

```rust
    /// Dump the document as a JSON value tree (for piping to external tools).
    Json {
        /// Path or http(s) URL of the PDF.
        input: String,
        /// Embed raw (still encoded) stream data as base64.
        #[arg(long, conflicts_with = "decode")]
        raw: bool,
        /// Embed decoded stream data as base64.
        #[arg(long)]
        decode: bool,
        /// Restrict logical elements to these 1-based pages (comma separated).
        #[arg(long, value_delimiter = ',')]
        pages: Option<Vec<usize>>,
        /// Skip the logical layer (pages/fonts/images/annotations).
        #[arg(long)]
        no_logical: bool,
        /// Include per-page content-stream operators (high volume).
        #[arg(long)]
        content_ops: bool,
    },
```

and replace `fn main()` with:

```rust
fn main() {
    let cli = Cli::parse();
    let result: Result<(), Failure> = match cli.command {
        Command::Info { file } => cmd_info(&file).map_err(Failure::from),
        Command::Text { file, page } => cmd_text(&file, page).map_err(Failure::from),
        Command::Render {
            file,
            page,
            out,
            scale,
            fonts,
            font_dir,
        } => cmd_render(&file, page, out, scale, fonts, font_dir).map_err(Failure::from),
        Command::Obj { file, num, gen } => {
            cmd_obj(&file, num, gen.unwrap_or(0)).map_err(Failure::from)
        }
        Command::Json {
            input,
            raw,
            decode,
            pages,
            no_logical,
            content_ops,
        } => {
            let flags = q::value::TreeFlags {
                raw,
                decode,
                pages,
                no_logical,
                content_ops,
            };
            json::cmd_json(&input, &flags).map_err(Failure::from)
        }
    };
    if let Err(failure) = result {
        eprintln!("pdfboss: {}", failure.message);
        std::process::exit(failure.code);
    }
}
```

- [ ] Add a clap parse test to the existing `mod tests` in `crates/pdfboss-cli/src/main.rs`:

```rust
    #[test]
    fn json_flags_parse() {
        let cli = Cli::parse_from([
            "pdfboss",
            "json",
            "in.pdf",
            "--raw",
            "--pages",
            "1,3",
            "--no-logical",
            "--content-ops",
        ]);
        let Command::Json {
            input,
            raw,
            decode,
            pages,
            no_logical,
            content_ops,
        } = cli.command
        else {
            panic!("expected json command");
        };
        assert_eq!(input, "in.pdf");
        assert!(raw && !decode && no_logical && content_ops);
        assert_eq!(pages, Some(vec![1, 3]));
    }
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli json_flags_parse` — expect pass.
- [ ] Manual smoke: run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo run -q -p pdfboss-cli -- json tests/fixtures/hello.pdf | head -20` from the repo root — expect output beginning `{` with `"header": {` and `"version": "1.7"`; then `... -- json tests/fixtures/hello.pdf --raw | grep -c '"data"'` — expect at least 1.
- [ ] Run `cargo fmt -p pdfboss-cli`, `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-cli --all-targets -- -D warnings`, then commit:

```bash
git add crates/pdfboss-cli/src/main.rs crates/pdfboss-cli/src/json.rs
git commit -m "feat(cli): add pdfboss json value-tree dump"
```

### Task 4: Hexdump formatting engine (`src/hexdump.rs`, formatting only)

**Files:**
- Create: `crates/pdfboss-cli/src/hexdump.rs`
- Modify: `crates/pdfboss-cli/src/main.rs` (add `mod hexdump;`)
- Test: unit tests inside `crates/pdfboss-cli/src/hexdump.rs`

**Interfaces:**
- Consumes: nothing beyond `std`.
- Produces (later tasks rely on these exactly):
  - `pub struct HexOpts { pub width: usize, pub color: bool }` (`Default` = 16, false)
  - `pub struct Mark { pub offset: u64, pub label: String }`
  - `pub fn hexdump(w: &mut impl io::Write, bytes: &[u8], base_offset: u64, opts: &HexOpts) -> io::Result<()>`
  - `pub fn hexdump_marked(w: &mut impl io::Write, bytes: &[u8], base_offset: u64, opts: &HexOpts, marks: &[Mark]) -> io::Result<()>` (marks must be sorted by offset)

Line format (pinned, golden tests rely on it): 8-digit lowercase hex offset, two spaces, `width` hex bytes each as `xx ` with one extra space after every 8th byte, one further space, then `|ascii|`. Partial rows pad the hex column so the ascii bar aligns. Byte classes: null (0x00) bright black `\x1b[90m`, whitespace (`\t \n \r` 0x0B 0x0C space) green `\x1b[32m`, printable graphic (0x21–0x7E) cyan `\x1b[36m`, everything else yellow `\x1b[33m`. Ascii column: graphic bytes as themselves, space as space, everything else `.`.

**Steps:**

- [ ] Add `mod hexdump;` to `crates/pdfboss-cli/src/main.rs` (before `mod input;`, keeping the list alphabetical) and create `crates/pdfboss-cli/src/hexdump.rs` with the doc comment, imports, and failing tests:

```rust
//! hexyl-style hexdump: offset gutter, hex columns, ascii column, byte-class
//! coloring, and labeled region boundaries. Also home of the `pdfboss hex`
//! subcommand (wired in a later task).

use std::fmt::Write as _;
use std::io;

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(bytes: &[u8], base: u64, opts: &HexOpts) -> String {
        let mut out = Vec::new();
        hexdump(&mut out, bytes, base, opts).expect("write to Vec cannot fail");
        String::from_utf8(out).expect("dump is valid text")
    }

    fn plain(width: usize) -> HexOpts {
        HexOpts {
            width,
            color: false,
        }
    }

    #[test]
    fn full_row_width_4() {
        assert_eq!(
            dump(b"ABCD", 0, &plain(4)),
            "00000000  41 42 43 44  |ABCD|\n"
        );
    }

    #[test]
    fn partial_row_pads_hex_column() {
        let expected = format!(
            "00000000  41 42 43 44  |ABCD|\n00000004  45{}|E|\n",
            " ".repeat(11)
        );
        assert_eq!(dump(b"ABCDE", 0, &plain(4)), expected);
    }

    #[test]
    fn sixteen_wide_rows_group_by_eight_and_use_the_base_offset() {
        assert_eq!(
            dump(b"0123456789abcdef", 0x1a40, &plain(16)),
            "00001a40  30 31 32 33 34 35 36 37  38 39 61 62 63 64 65 66  |0123456789abcdef|\n"
        );
    }

    #[test]
    fn second_row_offset_advances_by_width() {
        let out = dump(&[0u8; 17], 0, &plain(16));
        let second = out.lines().nth(1).expect("two rows");
        assert!(second.starts_with("00000010  "), "wrong offset: {second}");
    }

    #[test]
    fn ascii_column_shows_dots_for_non_printables_and_keeps_spaces() {
        assert_eq!(
            dump(&[0x00, b'\n', b'A', b' ', 0xff], 0, &plain(8)),
            format!("00000000  00 0a 41 20 ff{}|..A .|\n", " ".repeat(11))
        );
    }

    #[test]
    fn empty_input_dumps_nothing() {
        assert_eq!(dump(b"", 0, &plain(16)), "");
    }

    #[test]
    fn byte_classes_get_their_colors() {
        let colored = dump(
            &[0x00, b'\n', b'A', 0xff],
            0,
            &HexOpts {
                width: 4,
                color: true,
            },
        );
        assert!(
            colored.contains("\x1b[90m00\x1b[0m"),
            "null not bright black: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[32m0a\x1b[0m"),
            "whitespace not green: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[36m41\x1b[0m"),
            "printable not cyan: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[33mff\x1b[0m"),
            "other not yellow: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[36mA\x1b[0m"),
            "ascii column uncolored: {colored:?}"
        );
    }

    #[test]
    fn marks_print_before_the_row_containing_their_offset() {
        let marks = vec![
            Mark {
                offset: 0,
                label: "header".to_string(),
            },
            Mark {
                offset: 5,
                label: "obj 1 0".to_string(),
            },
        ];
        let mut out = Vec::new();
        hexdump_marked(&mut out, b"ABCDEFGH", 0, &plain(4), &marks).expect("writes");
        assert_eq!(
            String::from_utf8(out).expect("text"),
            "── header ──\n00000000  41 42 43 44  |ABCD|\n── obj 1 0 ──\n00000004  45 46 47 48  |EFGH|\n"
        );
    }

    #[test]
    fn marks_outside_the_dumped_range_are_dropped() {
        let marks = vec![
            Mark {
                offset: 2,
                label: "before".to_string(),
            },
            Mark {
                offset: 100,
                label: "after".to_string(),
            },
        ];
        let mut out = Vec::new();
        hexdump_marked(&mut out, b"ABCD", 4, &plain(4), &marks).expect("writes");
        let text = String::from_utf8(out).expect("text");
        assert!(!text.contains("before"), "mark before range kept: {text}");
        assert!(!text.contains("after"), "mark past range kept: {text}");
    }

    #[test]
    fn mark_at_end_offset_prints_after_the_last_row() {
        let marks = vec![Mark {
            offset: 4,
            label: "eof".to_string(),
        }];
        let mut out = Vec::new();
        hexdump_marked(&mut out, b"ABCD", 0, &plain(4), &marks).expect("writes");
        assert_eq!(
            String::from_utf8(out).expect("text"),
            "00000000  41 42 43 44  |ABCD|\n── eof ──\n"
        );
    }
}
```

(Padding arithmetic for the literals: each byte renders as three characters `xx `, each missing byte pads three spaces, and the row format adds one further space before `|`. A width-4 row whose only byte is `45` therefore has 1 + 9 + 1 = 11 spaces between `45` and `|E|`; the width-8 row with five bytes likewise has 1 + 9 + 1 = 11 spaces between `ff` and `|`.)

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli hexdump::` — expect a **compile error**: `cannot find struct 'HexOpts'` / `cannot find function 'hexdump'`.
- [ ] Implement the engine in `crates/pdfboss-cli/src/hexdump.rs`, inserted between the imports and the test module:

```rust
/// Hexdump options: bytes per row and ANSI coloring.
pub struct HexOpts {
    pub width: usize,
    pub color: bool,
}

impl Default for HexOpts {
    fn default() -> HexOpts {
        HexOpts {
            width: 16,
            color: false,
        }
    }
}

/// A labeled boundary printed when the dump reaches `offset`.
pub struct Mark {
    pub offset: u64,
    pub label: String,
}

/// Byte classes for coloring and the ascii column.
#[derive(Clone, Copy)]
enum ByteClass {
    Null,
    Printable,
    Whitespace,
    Other,
}

fn classify(b: u8) -> ByteClass {
    match b {
        0 => ByteClass::Null,
        b'\t' | b'\n' | b'\r' | 0x0B | 0x0C | b' ' => ByteClass::Whitespace,
        0x21..=0x7E => ByteClass::Printable,
        _ => ByteClass::Other,
    }
}

fn color_code(class: ByteClass) -> &'static str {
    match class {
        ByteClass::Null => "\x1b[90m",
        ByteClass::Printable => "\x1b[36m",
        ByteClass::Whitespace => "\x1b[32m",
        ByteClass::Other => "\x1b[33m",
    }
}

fn ascii_char(b: u8) -> char {
    match classify(b) {
        ByteClass::Printable => b as char,
        ByteClass::Whitespace if b == b' ' => ' ',
        ByteClass::Null | ByteClass::Whitespace | ByteClass::Other => '.',
    }
}

/// Dumps `bytes`, labeling the gutter as if the first byte sat at
/// `base_offset` in the file.
pub fn hexdump(
    w: &mut impl io::Write,
    bytes: &[u8],
    base_offset: u64,
    opts: &HexOpts,
) -> io::Result<()> {
    hexdump_marked(w, bytes, base_offset, opts, &[])
}

/// Like [`hexdump`], plus labeled boundary lines: before the row containing a
/// mark's offset a `── label ──` line is emitted (a mark at exactly the end
/// offset prints after the last row). `marks` must be sorted by offset; marks
/// outside `base_offset..=base_offset + bytes.len()` are dropped.
pub fn hexdump_marked(
    w: &mut impl io::Write,
    bytes: &[u8],
    base_offset: u64,
    opts: &HexOpts,
    marks: &[Mark],
) -> io::Result<()> {
    let width = opts.width.max(1);
    let end_offset = base_offset + bytes.len() as u64;
    let mut marks = marks
        .iter()
        .filter(|m| m.offset >= base_offset && m.offset <= end_offset)
        .peekable();
    let mut row_start = 0usize;
    while row_start < bytes.len() {
        let row_end = (row_start + width).min(bytes.len());
        let row_off_end = base_offset + row_end as u64;
        while marks.peek().is_some_and(|m| m.offset < row_off_end) {
            writeln!(w, "── {} ──", marks.next().expect("peeked").label)?;
        }
        write_row(
            w,
            &bytes[row_start..row_end],
            base_offset + row_start as u64,
            width,
            opts.color,
        )?;
        row_start = row_end;
    }
    for mark in marks {
        writeln!(w, "── {} ──", mark.label)?;
    }
    Ok(())
}

fn write_row(
    w: &mut impl io::Write,
    row: &[u8],
    offset: u64,
    width: usize,
    color: bool,
) -> io::Result<()> {
    const RESET: &str = "\x1b[0m";
    let mut hex = String::new();
    let mut ascii = String::new();
    for (i, &b) in row.iter().enumerate() {
        if i > 0 && i % 8 == 0 {
            hex.push(' ');
        }
        if color {
            let code = color_code(classify(b));
            let _ = write!(hex, "{code}{b:02x}{RESET} ");
            let _ = write!(ascii, "{code}{}{RESET}", ascii_char(b));
        } else {
            let _ = write!(hex, "{b:02x} ");
            ascii.push(ascii_char(b));
        }
    }
    for i in row.len()..width {
        if i > 0 && i % 8 == 0 {
            hex.push(' ');
        }
        hex.push_str("   ");
    }
    writeln!(w, "{offset:08x}  {hex} |{ascii}|")
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli hexdump::` — expect all 10 tests pass. If a literal mismatches by a space, the implementation (not the test) is wrong — the line format above is pinned.
- [ ] Run `cargo fmt -p pdfboss-cli`, `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-cli --all-targets -- -D warnings`, then commit:

```bash
git add crates/pdfboss-cli/src/main.rs crates/pdfboss-cli/src/hexdump.rs
git commit -m "feat(cli): hexyl-style hexdump engine"
```

### Task 5: `pdfboss hex` — selectors, `--annotate`, wiring

**Files:**
- Modify: `crates/pdfboss-cli/src/hexdump.rs` (append selector/annotation code and `cmd_hex`)
- Modify: `crates/pdfboss-cli/src/main.rs` (`Command` enum; `main()`)
- Test: unit tests inside `crates/pdfboss-cli/src/hexdump.rs`

**Interfaces:**
- Consumes: Task 1's `Input::{open, collect_elements, read_span, file_len}` and `use_color`; Task 4's `hexdump_marked`/`HexOpts`/`Mark`; `pdfboss_core::elements::{Element, ElementOpts, Span, XrefKind}`.
- Produces:
  - `pub enum Selector { WholeFile, Header, Trailer, Obj { num: u32, gen: Option<u16> }, Xref { index: usize }, Range { start: u64, end: u64 } }`
  - `pub fn parse_selector(s: &str) -> Result<Selector, String>`
  - `pub fn resolve_selector(sel: &Selector, elements: &[Element], file_len: u64) -> Result<Span, String>`
  - `pub fn element_marks(elements: &[Element]) -> Vec<Mark>`
  - `pub fn cmd_hex(input_spec: &str, selector: Option<&str>, annotate: bool, width: usize) -> Result<(), String>`
  - `Command::Hex { input: String, selector: Option<String>, annotate: bool, width: usize }` wired in `main()`

**Steps:**

- [ ] Extend the imports at the top of `crates/pdfboss-cli/src/hexdump.rs` to:

```rust
use std::fmt::Write as _;
use std::io;

use pdfboss_core::elements::{Element, ElementOpts, Span, XrefKind};

use crate::input::{use_color, Input};
```

and append the failing selector tests inside the existing `mod tests` block:

```rust
    use pdfboss_core::{Dict, Object, ObjRef};

    fn sample_elements() -> Vec<Element> {
        vec![
            Element::Header {
                version: (1, 7),
                span: Span { start: 0, end: 15 },
            },
            Element::IndirectObject {
                r: ObjRef { num: 1, gen: 0 },
                object: Object::Null,
                span: Span { start: 15, end: 60 },
                in_objstm: None,
            },
            Element::IndirectObject {
                r: ObjRef { num: 2, gen: 3 },
                object: Object::Null,
                span: Span { start: 60, end: 120 },
                in_objstm: Some((ObjRef { num: 9, gen: 0 }, Span { start: 4, end: 20 })),
            },
            Element::XrefSection {
                kind: XrefKind::Table,
                span: Span {
                    start: 120,
                    end: 180,
                },
                entries: 3,
            },
            Element::Trailer {
                dict: Dict::new(),
                span: Span {
                    start: 180,
                    end: 220,
                },
            },
            Element::StartXref {
                offset: 120,
                span: Span {
                    start: 220,
                    end: 235,
                },
            },
            Element::Eof {
                span: Span {
                    start: 235,
                    end: 241,
                },
            },
        ]
    }

    #[test]
    fn selectors_parse() {
        assert!(matches!(parse_selector("header"), Ok(Selector::Header)));
        assert!(matches!(parse_selector("trailer"), Ok(Selector::Trailer)));
        assert!(matches!(
            parse_selector("obj:12"),
            Ok(Selector::Obj { num: 12, gen: None })
        ));
        assert!(matches!(
            parse_selector("obj:12,0"),
            Ok(Selector::Obj {
                num: 12,
                gen: Some(0)
            })
        ));
        assert!(matches!(
            parse_selector("xref:0"),
            Ok(Selector::Xref { index: 0 })
        ));
        assert!(matches!(
            parse_selector("range:0x1A40-0x1B02"),
            Ok(Selector::Range {
                start: 0x1a40,
                end: 0x1b02
            })
        ));
        assert!(matches!(
            parse_selector("range:0-16"),
            Ok(Selector::Range { start: 0, end: 16 })
        ));
    }

    #[test]
    fn bad_selectors_error_with_guidance() {
        assert!(parse_selector("obj:x").is_err());
        assert!(parse_selector("obj:1,y").is_err());
        assert!(parse_selector("xref:z").is_err());
        assert!(parse_selector("range:5").is_err());
        assert!(parse_selector("range:9-5").is_err());
        let err = parse_selector("bogus").expect_err("unknown selector");
        assert!(err.contains("obj:N"), "no guidance in: {err}");
    }

    #[test]
    fn selectors_resolve_to_spans() {
        let elements = sample_elements();
        let resolve = |sel: &Selector| resolve_selector(sel, &elements, 241).expect("resolves");
        assert_eq!(resolve(&Selector::WholeFile), Span { start: 0, end: 241 });
        assert_eq!(resolve(&Selector::Header), Span { start: 0, end: 15 });
        assert_eq!(
            resolve(&Selector::Trailer),
            Span {
                start: 180,
                end: 220
            }
        );
        assert_eq!(
            resolve(&Selector::Obj { num: 1, gen: None }),
            Span { start: 15, end: 60 }
        );
        assert_eq!(
            resolve(&Selector::Obj {
                num: 2,
                gen: Some(3)
            }),
            Span { start: 60, end: 120 }
        );
        assert_eq!(
            resolve(&Selector::Xref { index: 0 }),
            Span {
                start: 120,
                end: 180
            }
        );
        assert_eq!(
            resolve(&Selector::Range { start: 3, end: 9 }),
            Span { start: 3, end: 9 }
        );
    }

    #[test]
    fn unresolvable_selectors_report_what_was_asked() {
        let elements = sample_elements();
        assert!(resolve_selector(&Selector::Obj { num: 99, gen: None }, &elements, 241).is_err());
        assert!(resolve_selector(
            &Selector::Obj {
                num: 1,
                gen: Some(7)
            },
            &elements,
            241
        )
        .is_err());
        assert!(resolve_selector(&Selector::Xref { index: 5 }, &elements, 241).is_err());
        assert!(resolve_selector(&Selector::Header, &[], 241).is_err());
        assert!(resolve_selector(&Selector::Trailer, &[], 241).is_err());
    }

    #[test]
    fn element_marks_label_physical_boundaries_in_offset_order() {
        let marks = element_marks(&sample_elements());
        let labels: Vec<&str> = marks.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "header",
                "obj 1 0",
                "xref table (3 entries)",
                "trailer",
                "startxref",
                "eof"
            ]
        );
        let offsets: Vec<u64> = marks.iter().map(|m| m.offset).collect();
        assert_eq!(offsets, vec![0, 15, 120, 180, 220, 235]);
    }

    #[test]
    fn objstm_members_produce_no_marks() {
        let marks = element_marks(&sample_elements());
        assert!(
            marks.iter().all(|m| !m.label.contains("obj 2 3")),
            "objstm member must not be labeled"
        );
    }
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli hexdump::` — expect a **compile error**: `cannot find type 'Selector'`.
- [ ] Append the selector code and subcommand to `crates/pdfboss-cli/src/hexdump.rs` (after `write_row`, before the test module):

```rust
/// What part of the file `pdfboss hex` dumps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Selector {
    WholeFile,
    Header,
    Trailer,
    Obj { num: u32, gen: Option<u16> },
    Xref { index: usize },
    Range { start: u64, end: u64 },
}

/// Parses `obj:12` / `obj:12,0` / `header` / `xref:0` / `trailer` /
/// `range:0x1A40-0x1B02` (offsets decimal or 0x-prefixed hex).
pub fn parse_selector(s: &str) -> Result<Selector, String> {
    if s == "header" {
        return Ok(Selector::Header);
    }
    if s == "trailer" {
        return Ok(Selector::Trailer);
    }
    if let Some(rest) = s.strip_prefix("obj:") {
        let (num, gen) = match rest.split_once(',') {
            None => (rest, None),
            Some((num, gen)) => (num, Some(gen)),
        };
        let num: u32 = num
            .trim()
            .parse()
            .map_err(|_| format!("bad object number in selector {s:?}"))?;
        let gen: Option<u16> = match gen {
            None => None,
            Some(gen) => Some(
                gen.trim()
                    .parse()
                    .map_err(|_| format!("bad generation in selector {s:?}"))?,
            ),
        };
        return Ok(Selector::Obj { num, gen });
    }
    if let Some(rest) = s.strip_prefix("xref:") {
        let index: usize = rest
            .trim()
            .parse()
            .map_err(|_| format!("bad xref index in selector {s:?}"))?;
        return Ok(Selector::Xref { index });
    }
    if let Some(rest) = s.strip_prefix("range:") {
        let (start, end) = rest
            .split_once('-')
            .ok_or_else(|| format!("range selector must look like range:0x1A40-0x1B02, got {s:?}"))?;
        let start = parse_offset(start)?;
        let end = parse_offset(end)?;
        if end < start {
            return Err(format!("range end {end:#x} precedes start {start:#x}"));
        }
        return Ok(Selector::Range { start, end });
    }
    Err(format!(
        "unknown selector {s:?}: expected obj:N[,G], header, xref:N, trailer, or range:START-END"
    ))
}

fn parse_offset(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|_| format!("bad offset {s:?}: expected decimal or 0x-prefixed hex"))
}

/// Maps a selector to a physical byte span using the element sequence.
/// For object-stream members `obj:` resolves to the container's span (that is
/// where the bytes live in the file). `xref:N` indexes sections in the order
/// they are yielded — xref-chain order, newest first (`xref:0` = newest).
pub fn resolve_selector(
    sel: &Selector,
    elements: &[Element],
    file_len: u64,
) -> Result<Span, String> {
    match sel {
        Selector::WholeFile => Ok(Span {
            start: 0,
            end: file_len,
        }),
        Selector::Range { start, end } => Ok(Span {
            start: *start,
            end: *end,
        }),
        Selector::Header => elements
            .iter()
            .find_map(|element| match element {
                Element::Header { span, .. } => Some(*span),
                _ => None,
            })
            .ok_or_else(|| "no header element found".to_string()),
        // The Trailer element is emitted once per document (merged dict, span
        // of the newest trailer region), so the first match is the only one.
        Selector::Trailer => elements
            .iter()
            .find_map(|element| match element {
                Element::Trailer { span, .. } => Some(*span),
                _ => None,
            })
            .ok_or_else(|| "no trailer element found".to_string()),
        Selector::Xref { index } => elements
            .iter()
            .filter_map(|element| match element {
                Element::XrefSection { span, .. } => Some(*span),
                _ => None,
            })
            .nth(*index)
            .ok_or_else(|| format!("no xref section {index}")),
        Selector::Obj { num, gen } => elements
            .iter()
            .find_map(|element| match element {
                Element::IndirectObject { r, span, .. }
                    if r.num == *num && gen.map_or(true, |g| g == r.gen) =>
                {
                    Some(*span)
                }
                _ => None,
            })
            .ok_or_else(|| match gen {
                Some(gen) => format!("object {num} {gen} not found"),
                None => format!("object {num} not found"),
            }),
    }
}

/// Boundary marks for `--annotate`: one labeled mark at every physical
/// element's span start, sorted by offset. The sort is load-bearing: xref
/// sections stream in chain order (newest first), not ascending file order,
/// and `hexdump_marked` requires offset-sorted marks. Object-stream members
/// are skipped (they would duplicate their container's boundary).
pub fn element_marks(elements: &[Element]) -> Vec<Mark> {
    let mut marks: Vec<Mark> = elements
        .iter()
        .filter_map(|element| match element {
            Element::Header { span, .. } => Some(Mark {
                offset: span.start,
                label: "header".to_string(),
            }),
            Element::IndirectObject {
                in_objstm: Some(_), ..
            } => None,
            Element::IndirectObject { r, span, .. } => Some(Mark {
                offset: span.start,
                label: format!("obj {} {}", r.num, r.gen),
            }),
            Element::XrefSection {
                kind,
                span,
                entries,
            } => {
                let kind = match kind {
                    XrefKind::Table => "table",
                    XrefKind::Stream => "stream",
                };
                Some(Mark {
                    offset: span.start,
                    label: format!("xref {kind} ({entries} entries)"),
                })
            }
            Element::Trailer { span, .. } => Some(Mark {
                offset: span.start,
                label: "trailer".to_string(),
            }),
            Element::StartXref { span, .. } => Some(Mark {
                offset: span.start,
                label: "startxref".to_string(),
            }),
            Element::Eof { span } => Some(Mark {
                offset: span.start,
                label: "eof".to_string(),
            }),
            Element::Page { .. }
            | Element::Font { .. }
            | Element::Image { .. }
            | Element::Annotation { .. }
            | Element::ContentOp { .. } => None,
        })
        .collect();
    marks.sort_by_key(|m| m.offset);
    marks
}

/// `pdfboss hex <file-or-url> [selector] [--annotate] [--width N]`.
pub fn cmd_hex(
    input_spec: &str,
    selector: Option<&str>,
    annotate: bool,
    width: usize,
) -> Result<(), String> {
    if width == 0 {
        return Err("--width must be at least 1".to_string());
    }
    let sel = match selector {
        Some(s) => parse_selector(s)?,
        None => Selector::WholeFile,
    };
    let input = Input::open(input_spec)?;
    let opts = ElementOpts {
        physical: true,
        logical: false,
        pages: None,
        content_ops: false,
    };
    let elements = input.collect_elements(opts);
    let span = resolve_selector(&sel, &elements, input.file_len())?;
    let bytes = input.read_span(span)?;
    let marks = if annotate {
        element_marks(&elements)
    } else {
        Vec::new()
    };
    let hex_opts = HexOpts {
        width,
        color: use_color(),
    };
    let stdout = io::stdout();
    let mut w = io::BufWriter::new(stdout.lock());
    hexdump_marked(&mut w, &bytes, span.start, &hex_opts, &marks).map_err(|e| e.to_string())
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli hexdump::` — expect all 16 tests pass.
- [ ] Wire the subcommand: append to the `Command` enum in `crates/pdfboss-cli/src/main.rs` (after `Json`):

```rust
    /// Hexdump the file or a selected element (hexyl-style).
    Hex {
        /// Path or http(s) URL of the PDF.
        input: String,
        /// obj:N[,G] | header | xref:N | trailer | range:START-END
        /// (offsets decimal or 0x-hex; xref sections indexed in chain
        /// order, newest first). Default: the whole file.
        selector: Option<String>,
        /// Print labeled element boundaries as the dump crosses them.
        #[arg(long)]
        annotate: bool,
        /// Bytes per row.
        #[arg(long, default_value_t = 16)]
        width: usize,
    },
```

and replace `fn main()` with the full new version:

```rust
fn main() {
    let cli = Cli::parse();
    let result: Result<(), Failure> = match cli.command {
        Command::Info { file } => cmd_info(&file).map_err(Failure::from),
        Command::Text { file, page } => cmd_text(&file, page).map_err(Failure::from),
        Command::Render {
            file,
            page,
            out,
            scale,
            fonts,
            font_dir,
        } => cmd_render(&file, page, out, scale, fonts, font_dir).map_err(Failure::from),
        Command::Obj { file, num, gen } => {
            cmd_obj(&file, num, gen.unwrap_or(0)).map_err(Failure::from)
        }
        Command::Json {
            input,
            raw,
            decode,
            pages,
            no_logical,
            content_ops,
        } => {
            let flags = q::value::TreeFlags {
                raw,
                decode,
                pages,
                no_logical,
                content_ops,
            };
            json::cmd_json(&input, &flags).map_err(Failure::from)
        }
        Command::Hex {
            input,
            selector,
            annotate,
            width,
        } => hexdump::cmd_hex(&input, selector.as_deref(), annotate, width).map_err(Failure::from),
    };
    if let Err(failure) = result {
        eprintln!("pdfboss: {}", failure.message);
        std::process::exit(failure.code);
    }
}
```

- [ ] Add a clap parse test to `mod tests` in `crates/pdfboss-cli/src/main.rs`:

```rust
    #[test]
    fn hex_flags_parse() {
        let cli = Cli::parse_from(["pdfboss", "hex", "in.pdf", "obj:12", "--annotate", "--width", "8"]);
        let Command::Hex {
            input,
            selector,
            annotate,
            width,
        } = cli.command
        else {
            panic!("expected hex command");
        };
        assert_eq!(input, "in.pdf");
        assert_eq!(selector.as_deref(), Some("obj:12"));
        assert!(annotate);
        assert_eq!(width, 8);
    }
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli hex_flags_parse` — expect pass.
- [ ] Manual smoke from the repo root: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo run -q -p pdfboss-cli -- hex tests/fixtures/hello.pdf header` — expect one/two rows starting `00000000  25 50 44 46` (`%PDF`); `... -- hex tests/fixtures/hello.pdf --annotate | grep -c '──'` — expect ≥ 8 (header, five objects, xref, trailer, startxref, eof).
- [ ] Run `cargo fmt -p pdfboss-cli`, `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-cli --all-targets -- -D warnings`, then commit:

```bash
git add crates/pdfboss-cli/src/main.rs crates/pdfboss-cli/src/hexdump.rs
git commit -m "feat(cli): add pdfboss hex with selectors and --annotate"
```

### Task 6: jq engine integration (`src/q/run.rs`)

This is the one task whose third-party API surface (jaq 2.x) may have drifted; it is deliberately self-contained, with a `cargo check` immediately after the skeleton so drift surfaces before anything builds on it. The public surface produced below is pinned; if jaq item paths differ on the ground, adapt **internals only** (consult `cargo doc -p jaq-core --no-deps` / docs.rs for the resolved versions) and keep `compile_program`/`run_program` exactly as declared. Do not downgrade to a pre-2.0 jaq generation.

**Files:**
- Modify: `crates/pdfboss-cli/Cargo.toml` (dependencies)
- Modify: `crates/pdfboss-cli/src/q/mod.rs` (add `pub mod run;`)
- Create: `crates/pdfboss-cli/src/q/run.rs`
- Test: unit tests inside `crates/pdfboss-cli/src/q/run.rs`

**Interfaces:**
- Consumes: `jaq_core::load::{Arena, File, Loader}`, `jaq_core::{Compiler, Ctx, RcIter}`, `jaq_std::{defs, funs}`, `jaq_json::{defs, funs, Val}` (with the `serde_json` feature: `Val: From<serde_json::Value>` and `serde_json::Value: From<Val>`).
- Produces (Task 7 relies on these exactly):
  - `pub struct Program` (opaque, holds the compiled filter)
  - `pub fn compile_program(code: &str) -> Result<Program, String>` — error strings prefixed `jq:` and carrying byte positions
  - `pub fn run_program(program: &Program, input: serde_json::Value) -> Vec<Result<serde_json::Value, String>>`

**Steps:**

- [ ] Add the jaq dependencies to `crates/pdfboss-cli/Cargo.toml`, in the `[dependencies]` section after `futures-core = "0.3"`:

```toml
jaq-core = "2"
jaq-std = "2"
jaq-json = { version = "1", features = ["serde_json"] }
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo check -p pdfboss-cli` — expect success (deps resolve and nothing else changed yet). Note the resolved versions with `cargo tree -p pdfboss-cli -i jaq-core --depth 0` for the record.
- [ ] Change `crates/pdfboss-cli/src/q/mod.rs` to its full new content:

```rust
//! `pdfboss q`: document to JSON value tree, and the jq engine that queries it.

pub mod run;
pub mod value;
```

and create `crates/pdfboss-cli/src/q/run.rs` with the **complete** engine integration plus its tests in one step (this is the focused-verification task; the very next step is the drift check):

```rust
//! Compiling and running jq programs (via the jaq engine) over the value
//! tree. Compile errors are reported with byte positions and become exit
//! code 2 in `cmd_q` (Task 7), distinct from PDF errors (exit code 1).

use std::fmt::Write as _;

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, RcIter};
use jaq_json::Val;
use serde_json::Value;

/// A compiled jq program, ready to run over any number of inputs.
pub struct Program {
    filter: jaq_core::Filter<jaq_core::Native<Val>>,
}

/// Compiles `code` against the jq standard library, reporting lex/parse/
/// compile errors with byte positions.
pub fn compile_program(code: &str) -> Result<Program, String> {
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let arena = Arena::default();
    let modules = loader
        .load(&arena, File { path: (), code })
        .map_err(|errors| describe_load_errors(code, errors))?;
    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|errors| describe_compile_errors(code, errors))?;
    Ok(Program { filter })
}

/// Runs the program over one input value, collecting every output in order.
/// Runtime errors (e.g. `error("boom")`) come back as `Err` items.
pub fn run_program(program: &Program, input: Value) -> Vec<Result<Value, String>> {
    let inputs = RcIter::new(core::iter::empty());
    program
        .filter
        .run((Ctx::new([], &inputs), Val::from(input)))
        .map(|item| item.map(Value::from).map_err(|e| format!("{e}")))
        .collect()
}

/// Byte offset of `part` (a slice borrowed from `code`) within `code`.
fn offset_in(code: &str, part: &str) -> usize {
    (part.as_ptr() as usize).saturating_sub(code.as_ptr() as usize)
}

fn describe_load_errors(code: &str, errors: jaq_core::load::Errors<&str, ()>) -> String {
    let mut out = String::new();
    for (file, error) in errors {
        let _ = file;
        match error {
            jaq_core::load::Error::Io(items) => {
                for (path, message) in items {
                    push_error(&mut out, &format!("io error ({path}): {message}"));
                }
            }
            jaq_core::load::Error::Lex(items) => {
                for (expected, found) in items {
                    push_error(
                        &mut out,
                        &format!(
                            "lex error at byte {}: expected {}",
                            offset_in(code, found),
                            expected.as_str()
                        ),
                    );
                }
            }
            jaq_core::load::Error::Parse(items) => {
                for (expected, found) in items {
                    push_error(
                        &mut out,
                        &format!(
                            "parse error at byte {}: expected {}",
                            offset_in(code, found),
                            expected.as_str()
                        ),
                    );
                }
            }
        }
    }
    if out.is_empty() {
        out.push_str("jq: invalid program");
    }
    out
}

fn describe_compile_errors(code: &str, errors: jaq_core::compile::Errors<&str, ()>) -> String {
    let mut out = String::new();
    for (file, file_errors) in errors {
        let _ = file;
        for (found, undefined) in file_errors {
            push_error(
                &mut out,
                &format!(
                    "compile error at byte {}: undefined {}",
                    offset_in(code, found),
                    undefined.as_str()
                ),
            );
        }
    }
    if out.is_empty() {
        out.push_str("jq: invalid program");
    }
    out
}

fn push_error(out: &mut String, message: &str) {
    if !out.is_empty() {
        out.push_str("; ");
    }
    let _ = write!(out, "jq: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity_program_round_trips() {
        let program = compile_program(".").expect("identity compiles");
        let input = json!({"a": 1});
        assert_eq!(run_program(&program, input.clone()), vec![Ok(input)]);
    }

    #[test]
    fn programs_can_produce_multiple_outputs() {
        let program = compile_program(".[] | . + 1").expect("compiles");
        assert_eq!(
            run_program(&program, json!([1, 2])),
            vec![Ok(json!(2)), Ok(json!(3))]
        );
    }

    #[test]
    fn field_and_index_access_work_over_objects() {
        let program = compile_program(r#".objects["12 0"]._span"#).expect("compiles");
        let input = json!({"objects": {"12 0": {"_span": [1, 2]}}});
        assert_eq!(run_program(&program, input), vec![Ok(json!([1, 2]))]);
    }

    #[test]
    fn std_library_functions_are_available() {
        let program = compile_program("[.[] | select(. > 1)] | length").expect("std defs loaded");
        assert_eq!(run_program(&program, json!([1, 2, 3])), vec![Ok(json!(2))]);
    }

    #[test]
    fn parse_error_reports_byte_position_with_jq_prefix() {
        let err = compile_program(".foo|").expect_err("trailing pipe is invalid");
        assert!(err.starts_with("jq:"), "no jq prefix in: {err}");
        assert!(err.contains("byte"), "no position in: {err}");
    }

    #[test]
    fn undefined_names_are_compile_errors() {
        let err = compile_program("nosuchfilter").expect_err("undefined filter");
        assert!(err.contains("undefined"), "wrong message: {err}");
    }

    #[test]
    fn runtime_errors_come_back_as_err_items() {
        let program = compile_program(r#"error("boom")"#).expect("compiles");
        let out = run_program(&program, json!(null));
        assert_eq!(out.len(), 1);
        let err = out[0].as_ref().expect_err("runtime error expected");
        assert!(err.contains("boom"), "message lost: {err}");
    }
}
```

- [ ] **Drift check, immediately:** run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo check -p pdfboss-cli` — expect success. If it fails inside `q/run.rs`, the jaq API generation differs from the code above; known adaptation points, in likelihood order: (a) `jaq_std::funs()` may need a turbofish — `jaq_std::funs::<Val>()`; (b) the error tuple shapes in `load::Error::Lex/Parse` and `compile::Errors` may destructure differently — match what `cargo doc -p jaq-core --no-deps` shows, keeping the "byte offset via `offset_in`" reporting; (c) `File { path: (), code }` field names. Adapt internals only; the `compile_program`/`run_program` signatures and test expectations stay.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli q::run::` — expect all 7 tests pass.
- [ ] Run `cargo fmt -p pdfboss-cli`, `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-cli --all-targets -- -D warnings`, then commit:

```bash
git add crates/pdfboss-cli/Cargo.toml crates/pdfboss-cli/src/q
git commit -m "feat(cli): compile and run jq programs via the jaq engine"
```

### Task 7: `pdfboss q` — wiring, `-r`, `--hex`

**Files:**
- Modify: `crates/pdfboss-cli/src/q/run.rs` (append `cmd_q` and `result_spans`)
- Modify: `crates/pdfboss-cli/src/main.rs` (`Command` enum; `main()`)
- Test: unit tests inside `crates/pdfboss-cli/src/q/run.rs`

**Interfaces:**
- Consumes: Task 1's `Input`/`use_color`/`Failure`; Task 2's `TreeFlags`/`build_tree`; Task 3's `write_json_pretty`; Task 4's `hexdump`/`HexOpts`; Task 6's `compile_program`/`run_program`; `pdfboss_core::elements::Span`.
- Produces:
  - `pub fn cmd_q(input_spec: &str, program: &str, flags: &TreeFlags, hex: bool, raw_strings: bool) -> Result<(), crate::Failure>`
  - `Command::Q { input: String, program: String, raw: bool, decode: bool, hex: bool, raw_strings: bool, pages: Option<Vec<usize>>, no_logical: bool, content_ops: bool }` wired in `main()`

**Steps:**

- [ ] Append failing tests for `result_spans` inside the existing `mod tests` block of `crates/pdfboss-cli/src/q/run.rs`:

```rust
    #[test]
    fn span_objects_are_detected_for_hex_mode() {
        let one = json!({"_span": [10, 20], "_kind": "object"});
        assert_eq!(
            result_spans(&one),
            Some(vec![Span { start: 10, end: 20 }])
        );
        let many = json!([{"_span": [0, 5]}, {"_span": [5, 9]}]);
        assert_eq!(
            result_spans(&many),
            Some(vec![
                Span { start: 0, end: 5 },
                Span { start: 5, end: 9 }
            ])
        );
    }

    #[test]
    fn non_span_results_fall_back_to_json() {
        assert_eq!(result_spans(&json!(42)), None);
        assert_eq!(result_spans(&json!({"span": [1, 2]})), None);
        assert_eq!(result_spans(&json!({"_span": [1]})), None);
        assert_eq!(result_spans(&json!({"_span": ["a", "b"]})), None);
        assert_eq!(result_spans(&json!({"_span": [9, 5]})), None);
        assert_eq!(result_spans(&json!([])), None);
        assert_eq!(
            result_spans(&json!([{"_span": [0, 5]}, 7])),
            None,
            "mixed arrays are not hexdumped"
        );
    }
```

and add `use pdfboss_core::elements::Span;` to the test module's imports (next to `use serde_json::json;`).

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli q::run::` — expect a **compile error**: `cannot find function 'result_spans'`.
- [ ] Append the subcommand to `crates/pdfboss-cli/src/q/run.rs` (after `push_error`, before the test module), and extend the file's imports to:

```rust
use std::fmt::Write as _;
use std::io::Write as _;

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, RcIter};
use jaq_json::Val;
use pdfboss_core::elements::Span;
use pdfboss_core::Stream;
use serde_json::Value;

use crate::hexdump::{hexdump, HexOpts};
use crate::input::{use_color, Input};
use crate::json::write_json_pretty;
use crate::q::value::{build_tree, TreeFlags};
use crate::Failure;
```

New code:

```rust
/// `pdfboss q <file-or-url> '<program>' [--raw|--decode] [--hex] [-r]
/// [--pages ..]`: run a jq program over the value tree. Program errors exit
/// 2; PDF/IO and jq runtime errors exit 1.
pub fn cmd_q(
    input_spec: &str,
    program: &str,
    flags: &TreeFlags,
    hex: bool,
    raw_strings: bool,
) -> Result<(), Failure> {
    // Compile first: a bad program should fail fast, before any I/O.
    let program = compile_program(program).map_err(Failure::program)?;
    let input = Input::open(input_spec).map_err(Failure::new)?;
    let opts = flags.element_opts().map_err(Failure::new)?;
    let elements = input.collect_elements(opts);
    let mut decode = |s: &Stream| input.decode_stream(s);
    let tree = build_tree(&elements, flags.stream_data(), flags.content_ops, &mut decode);
    let results = run_program(&program, tree);

    let color = use_color();
    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::new(stdout.lock());
    for result in results {
        let value = result.map_err(|message| Failure::new(format!("jq: {message}")))?;
        if hex {
            if let Some(spans) = result_spans(&value) {
                for span in spans {
                    let bytes = input.read_span(span).map_err(Failure::new)?;
                    writeln!(w, "── {:#x}..{:#x} ──", span.start, span.end)
                        .map_err(io_failure)?;
                    let hex_opts = HexOpts {
                        width: 16,
                        color,
                    };
                    hexdump(&mut w, &bytes, span.start, &hex_opts).map_err(io_failure)?;
                }
                continue;
            }
        }
        if raw_strings {
            if let Value::String(s) = &value {
                writeln!(w, "{s}").map_err(io_failure)?;
                continue;
            }
        }
        let mut text = String::new();
        write_json_pretty(&mut text, &value, 0, color);
        writeln!(w, "{text}").map_err(io_failure)?;
    }
    Ok(())
}

fn io_failure(e: std::io::Error) -> Failure {
    Failure::new(e.to_string())
}

/// For `--hex`: if `v` is an object with a two-element numeric `_span`, or a
/// non-empty array made entirely of such objects, the spans to hexdump.
fn result_spans(v: &Value) -> Option<Vec<Span>> {
    fn one(v: &Value) -> Option<Span> {
        let span = v.as_object()?.get("_span")?.as_array()?;
        if span.len() != 2 {
            return None;
        }
        let start = span[0].as_u64()?;
        let end = span[1].as_u64()?;
        (end >= start).then_some(Span { start, end })
    }
    match v {
        Value::Object(_) => one(v).map(|span| vec![span]),
        Value::Array(items) if !items.is_empty() => {
            items.iter().map(one).collect::<Option<Vec<Span>>>()
        }
        _ => None,
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli q::run::` — expect all 9 tests pass.
- [ ] Wire the subcommand: append to the `Command` enum in `crates/pdfboss-cli/src/main.rs` (after `Hex`):

```rust
    /// Run a jq program over the document's JSON value tree.
    Q {
        /// Path or http(s) URL of the PDF.
        input: String,
        /// jq program, e.g. '.objects["12 0"]'.
        program: String,
        /// Embed raw (still encoded) stream data as base64.
        #[arg(long, conflicts_with = "decode")]
        raw: bool,
        /// Embed decoded stream data as base64.
        #[arg(long)]
        decode: bool,
        /// Hexdump results carrying a `_span` instead of printing JSON.
        #[arg(long)]
        hex: bool,
        /// Print string results raw, without quotes (like jq -r).
        #[arg(short = 'r')]
        raw_strings: bool,
        /// Restrict logical elements to these 1-based pages (comma separated).
        #[arg(long, value_delimiter = ',')]
        pages: Option<Vec<usize>>,
        /// Skip the logical layer (pages/fonts/images/annotations).
        #[arg(long)]
        no_logical: bool,
        /// Include per-page content-stream operators (high volume).
        #[arg(long)]
        content_ops: bool,
    },
```

and replace `fn main()` with its final full version (the `Q` arm already returns `Result<(), Failure>`, so no `map_err`):

```rust
fn main() {
    let cli = Cli::parse();
    let result: Result<(), Failure> = match cli.command {
        Command::Info { file } => cmd_info(&file).map_err(Failure::from),
        Command::Text { file, page } => cmd_text(&file, page).map_err(Failure::from),
        Command::Render {
            file,
            page,
            out,
            scale,
            fonts,
            font_dir,
        } => cmd_render(&file, page, out, scale, fonts, font_dir).map_err(Failure::from),
        Command::Obj { file, num, gen } => {
            cmd_obj(&file, num, gen.unwrap_or(0)).map_err(Failure::from)
        }
        Command::Json {
            input,
            raw,
            decode,
            pages,
            no_logical,
            content_ops,
        } => {
            let flags = q::value::TreeFlags {
                raw,
                decode,
                pages,
                no_logical,
                content_ops,
            };
            json::cmd_json(&input, &flags).map_err(Failure::from)
        }
        Command::Hex {
            input,
            selector,
            annotate,
            width,
        } => hexdump::cmd_hex(&input, selector.as_deref(), annotate, width).map_err(Failure::from),
        Command::Q {
            input,
            program,
            raw,
            decode,
            hex,
            raw_strings,
            pages,
            no_logical,
            content_ops,
        } => {
            let flags = q::value::TreeFlags {
                raw,
                decode,
                pages,
                no_logical,
                content_ops,
            };
            q::run::cmd_q(&input, &program, &flags, hex, raw_strings)
        }
    };
    if let Err(failure) = result {
        eprintln!("pdfboss: {}", failure.message);
        std::process::exit(failure.code);
    }
}
```

- [ ] Add a clap parse test to `mod tests` in `crates/pdfboss-cli/src/main.rs`:

```rust
    #[test]
    fn q_flags_parse() {
        let cli = Cli::parse_from(["pdfboss", "q", "in.pdf", ".header", "--hex", "-r"]);
        let Command::Q {
            input,
            program,
            raw,
            decode,
            hex,
            raw_strings,
            ..
        } = cli.command
        else {
            panic!("expected q command");
        };
        assert_eq!(input, "in.pdf");
        assert_eq!(program, ".header");
        assert!(hex && raw_strings);
        assert!(!raw && !decode);
    }
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli q_flags_parse` — expect pass.
- [ ] Manual smoke from the repo root: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo run -q -p pdfboss-cli -- q tests/fixtures/hello.pdf '.header.version' -r` — expect `1.7`; `... -- q tests/fixtures/hello.pdf '.header' --hex` — expect a `── 0x0..0xf ──` heading followed by a hex row starting `00000000  25 50 44 46`; `... -- q tests/fixtures/hello.pdf '.foo|'; echo "exit=$?"` — expect a `pdfboss: jq: …byte…` line on stderr and `exit=2`.
- [ ] Run `cargo fmt -p pdfboss-cli`, `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-cli --all-targets -- -D warnings`, then commit:

```bash
git add crates/pdfboss-cli/src/main.rs crates/pdfboss-cli/src/q
git commit -m "feat(cli): add pdfboss q with -r and --hex span dumps"
```

### Task 8: Golden integration tests and final sweep

**Files:**
- Create: `crates/pdfboss-cli/tests/common/mod.rs`, `crates/pdfboss-cli/tests/json_cmd.rs`, `crates/pdfboss-cli/tests/hex_cmd.rs`, `crates/pdfboss-cli/tests/q_cmd.rs`
- Create (blessed, then committed): `crates/pdfboss-cli/tests/golden/{json-hello.txt, hex-header.txt, hex-annotate.txt, hex-obj-3.txt, hex-trailer.txt, hex-xref-stream.txt, hex-range.txt, q-object-3-0.txt, q-select-kind.txt, q-hex-header.txt}`
- Modify: `crates/pdfboss-cli/Cargo.toml` (description string only)
- Test: the three new integration test binaries; full crate suite

**Interfaces:**
- Consumes: the `pdfboss` binary via `CARGO_BIN_EXE_pdfboss` (same pattern as `crates/pdfboss-cli/tests/cli.rs:13-18`); committed fixtures `tests/fixtures/hello.pdf` (objects 1 catalog, 2 pages, 3 page, 4 content stream, 5 font; version 1.7) and `tests/fixtures/xref-stream.pdf` (object stream 6 holding 1, 2, 3, 5; xref stream); `pdfboss_testkit::multi_page_doc`.
- Produces: test helpers `fixture(name: &str) -> PathBuf`, `pdfboss(args: &[&str]) -> Output`, `stdout_str(&Output) -> String`, `strip_ansi(&str) -> String`, `assert_golden(name: &str, actual: &str)` in `tests/common/mod.rs`. Golden files are blessed via `UPDATE_GOLDENS=1`, reviewed by hand, and committed; tests compare color-stripped output byte-for-byte.

**Steps:**

- [ ] Create `crates/pdfboss-cli/tests/common/mod.rs`:

```rust
//! Shared helpers for the explorer subcommand integration tests.

#![allow(dead_code)] // each test binary uses a subset

use std::path::PathBuf;
use std::process::{Command, Output};

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Runs the pdfboss binary with `NO_COLOR` set (belt and braces: piped
/// output is already colorless, but golden comparisons must never depend on
/// the environment).
pub fn pdfboss(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfboss"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to launch pdfboss binary")
}

pub fn stdout_str(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Removes ANSI CSI escape sequences (`ESC [ … <alpha>`).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Compares `actual` against `tests/golden/<name>`. Bless (re)creates the
/// file when `UPDATE_GOLDENS` is set; review the diff before committing.
pub fn assert_golden(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().expect("golden files live in a directory"))
            .expect("create golden dir");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "golden file {} missing; bless with UPDATE_GOLDENS=1",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "output differs from {}; review and re-bless with UPDATE_GOLDENS=1",
        path.display()
    );
}
```

- [ ] Create `crates/pdfboss-cli/tests/json_cmd.rs`:

```rust
//! End-to-end tests for `pdfboss json`.

mod common;

use common::{assert_golden, fixture, pdfboss, stdout_str, strip_ansi};

#[test]
fn json_hello_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap()]);
    assert!(output.status.success(), "json failed: {output:?}");
    assert_golden("json-hello.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn json_dump_is_stable_across_runs() {
    let file = fixture("hello.pdf");
    let first = pdfboss(&["json", file.to_str().unwrap()]);
    let second = pdfboss(&["json", file.to_str().unwrap()]);
    assert!(first.status.success() && second.status.success());
    assert_eq!(
        stdout_str(&first),
        stdout_str(&second),
        "json dump must be deterministic"
    );
}

#[test]
fn json_raw_embeds_stream_data() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--raw"]);
    assert!(output.status.success(), "json --raw failed: {output:?}");
    assert!(stdout_str(&output).contains("\"data\""), "no data field");
}

#[test]
fn json_decode_embeds_decoded_stream_data() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--decode"]);
    assert!(output.status.success(), "json --decode failed: {output:?}");
    assert!(stdout_str(&output).contains("\"data\""), "no data field");
}

#[test]
fn json_no_logical_empties_pages() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--no-logical"]);
    assert!(output.status.success(), "json failed: {output:?}");
    assert!(
        stdout_str(&output).contains("\"pages\": []"),
        "pages not empty: {}",
        stdout_str(&output)
    );
}

#[test]
fn json_content_ops_lists_operator_spans() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--content-ops"]);
    assert!(output.status.success(), "json failed: {output:?}");
    let text = stdout_str(&output);
    assert!(text.contains("\"content_ops\""), "no content_ops: {text}");
    assert!(
        text.contains("\"_span_in_content\""),
        "no op spans: {text}"
    );
}

#[test]
fn json_pages_filter_keeps_only_selected_pages() {
    let bytes = pdfboss_testkit::multi_page_doc(&["one", "two", "three"]);
    let path = std::env::temp_dir().join(format!(
        "pdfboss-json-pages-{}.pdf",
        std::process::id()
    ));
    std::fs::write(&path, bytes).expect("write temp fixture");
    let output = pdfboss(&["json", path.to_str().unwrap(), "--pages", "2"]);
    let _ = std::fs::remove_file(&path);
    assert!(output.status.success(), "json failed: {output:?}");
    let text = stdout_str(&output);
    assert!(text.contains("\"index\": 1"), "page 2 missing: {text}");
    assert!(!text.contains("\"index\": 0"), "page 1 kept: {text}");
    assert!(!text.contains("\"index\": 2"), "page 3 kept: {text}");
}

#[test]
fn json_missing_file_exits_one() {
    let output = pdfboss(&["json", "definitely-not-here.pdf"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}

#[test]
fn json_raw_and_decode_conflict_is_a_usage_error() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--raw", "--decode"]);
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
}
```

- [ ] Create `crates/pdfboss-cli/tests/hex_cmd.rs`:

```rust
//! End-to-end tests for `pdfboss hex`.

mod common;

use common::{assert_golden, fixture, pdfboss, stdout_str, strip_ansi};

#[test]
fn hex_header_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "header"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-header.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_whole_file_annotated_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "--annotate"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    let text = strip_ansi(&stdout_str(&output));
    assert!(text.contains("── obj 3 0 ──"), "no object boundary: {text}");
    assert!(text.contains("── trailer ──"), "no trailer boundary: {text}");
    assert_golden("hex-annotate.txt", &text);
}

#[test]
fn hex_obj_selector_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "obj:3"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-obj-3.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_obj_with_generation_matches_bare_obj() {
    let file = fixture("hello.pdf");
    let bare = pdfboss(&["hex", file.to_str().unwrap(), "obj:3"]);
    let with_gen = pdfboss(&["hex", file.to_str().unwrap(), "obj:3,0"]);
    assert!(bare.status.success() && with_gen.status.success());
    assert_eq!(stdout_str(&bare), stdout_str(&with_gen));
}

#[test]
fn hex_trailer_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "trailer"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-trailer.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_xref_selector_on_xref_stream_fixture_matches_golden() {
    let file = fixture("xref-stream.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "xref:0", "--annotate"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-xref-stream.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_range_with_width_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&[
        "hex",
        file.to_str().unwrap(),
        "range:0x0-0x10",
        "--width",
        "8",
    ]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-range.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_range_accepts_decimal_offsets() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "range:0-8"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    // %PDF-1.7
    assert!(
        stdout_str(&output).contains("25 50 44 46"),
        "no %PDF bytes: {}",
        stdout_str(&output)
    );
}

#[test]
fn hex_bad_selector_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "bogus"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}

#[test]
fn hex_missing_object_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "obj:999"]);
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("999"), "unhelpful error: {err}");
}
```

- [ ] Create `crates/pdfboss-cli/tests/q_cmd.rs`:

```rust
//! End-to-end tests for `pdfboss q`.

mod common;

use common::{assert_golden, fixture, pdfboss, stdout_str, strip_ansi};

#[test]
fn q_object_three_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), r#".objects["3 0"]"#]);
    assert!(output.status.success(), "q failed: {output:?}");
    assert_golden("q-object-3-0.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn q_select_over_kind_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&[
        "q",
        file.to_str().unwrap(),
        r#"[.objects[] | select(._kind == "object") | ._ref[0]] | sort"#,
    ]);
    assert!(output.status.success(), "q failed: {output:?}");
    assert_golden("q-select-kind.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn q_hex_dumps_span_ranges_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), ".header", "--hex"]);
    assert!(output.status.success(), "q failed: {output:?}");
    let text = strip_ansi(&stdout_str(&output));
    assert!(text.starts_with("── 0x0..0x"), "no range heading: {text}");
    assert_golden("q-hex-header.txt", &text);
}

#[test]
fn q_raw_strings_print_unquoted() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), ".header.version", "-r"]);
    assert!(output.status.success(), "q failed: {output:?}");
    assert_eq!(stdout_str(&output), "1.7\n");
}

#[test]
fn q_objstm_members_expose_their_container() {
    let file = fixture("xref-stream.pdf");
    let output = pdfboss(&[
        "q",
        file.to_str().unwrap(),
        r#".objects["1 0"]._objstm._r"#,
    ]);
    assert!(output.status.success(), "q failed: {output:?}");
    assert_eq!(strip_ansi(&stdout_str(&output)), "[\n  6,\n  0\n]\n");
}

#[test]
fn q_compile_error_exits_two_with_position() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), ".foo|"]);
    assert_eq!(output.status.code(), Some(2), "program errors exit 2");
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("jq"), "no jq marker: {err}");
    assert!(err.contains("byte"), "no position: {err}");
}

#[test]
fn q_runtime_error_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), r#"error("boom")"#]);
    assert_eq!(output.status.code(), Some(1), "runtime errors exit 1");
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("boom"), "message lost: {err}");
}

#[test]
fn q_missing_file_exits_one() {
    let output = pdfboss(&["q", "definitely-not-here.pdf", "."]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli --test json_cmd --test hex_cmd --test q_cmd` — expect the golden-backed tests to **fail** with `golden file … missing; bless with UPDATE_GOLDENS=1` and the direct-assertion tests to pass. Any direct-assertion failure is a real bug in Tasks 1–7: stop and fix it first (use the superpowers:systematic-debugging skill).
- [ ] Bless: `UPDATE_GOLDENS=1 CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli --test json_cmd --test hex_cmd --test q_cmd` — expect all pass (bless mode writes and returns).
- [ ] Review the blessed files before trusting them (they are now the contract):

```bash
grep -c '"_span"' crates/pdfboss-cli/tests/golden/json-hello.txt        # expect >= 6 (5 objects + header)
grep '"startxref"' crates/pdfboss-cli/tests/golden/json-hello.txt       # expect a number, not null
grep '"3 0"' crates/pdfboss-cli/tests/golden/json-hello.txt             # expect the page object key
head -1 crates/pdfboss-cli/tests/golden/hex-header.txt                  # expect: 00000000  25 50 44 46 ...
grep -c '──' crates/pdfboss-cli/tests/golden/hex-annotate.txt           # expect >= 8
grep 'xref stream' crates/pdfboss-cli/tests/golden/hex-xref-stream.txt  # expect the stream-kind label
head -1 crates/pdfboss-cli/tests/golden/q-hex-header.txt                # expect: ── 0x0..0x... ──
cat crates/pdfboss-cli/tests/golden/q-select-kind.txt                   # expect a sorted JSON array of 1..5
```

If anything looks wrong (missing keys, empty dumps, unlabeled boundaries), the bug is in Tasks 1–7 — fix it there, re-bless, and re-review.

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli --test json_cmd --test hex_cmd --test q_cmd` (no bless env) — expect everything green against the committed goldens.
- [ ] Update the crate description in `crates/pdfboss-cli/Cargo.toml` to reflect the new surface:

```toml
description = "Command-line interface for pdfboss: info, text, render, obj, json, hex and q subcommands"
```

- [ ] Final sweep: run

```bash
cargo fmt --all
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-cli --all-targets -- -D warnings
CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli
```

— expect no diffs, no lints, full suite green (including the untouched `tests/cli.rs`).

- [ ] Commit:

```bash
git add crates/pdfboss-cli/Cargo.toml crates/pdfboss-cli/tests
git commit -m "test(cli): golden coverage for q, hex and json subcommands"
```

---

## Completion checklist (verify before calling the plan done)

- [ ] `pdfboss json <file-or-url> [--raw|--decode] [--pages ..] [--no-logical] [--content-ops]` dumps the full tree; two consecutive runs are byte-identical.
- [ ] `pdfboss hex <file-or-url> [selector] [--annotate] [--width N]` supports `obj:12`, `obj:12,0`, `header`, `xref:0`, `trailer`, `range:0x1A40-0x1B02`, and whole-file default; colors auto-disable when piped or `NO_COLOR` is set.
- [ ] `pdfboss q <file-or-url> '<program>' [--raw|--decode] [--hex] [-r] [--pages ..]` runs jaq over the tree; compile errors carry byte positions and exit 2; `--hex` dumps `_span` ranges.
- [ ] The value tree matches the spec example exactly: `header.version`/`_span`/`_kind`; `objects` keyed `"N G"` with `_kind`/`_ref`/`_span`/`_objstm`/`value`; `{"_r": [num, gen]}` refs; names as strings; UTF-8-or-`_bytes` strings; `_stream` with `dict`/`length` and mode-dependent `data`; `pages` with `index`/`_ref`/`fonts`/`images`/`annotations`; `xref` entries with plain `kind`/`entries`/`_span`; `trailer` with `_span`/`value`; numeric `startxref`.
- [ ] `serde_json`/jaq appear only in `crates/pdfboss-cli/Cargo.toml`; `git diff --stat` shows no changes under `crates/pdfboss-core`.
- [ ] No `tui` subcommand, no `pdfboss-tui` dependency (plan 05).
- [ ] Existing `info`/`text`/`render`/`obj` tests pass unmodified; clippy/fmt clean.




