# pdfboss-core Element Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A lazy `Document::elements(opts)` iterator in `pdfboss-core` yielding every physical element (header, indirect objects, xref sections, trailer, startxref, eof — all with byte spans) and logical element (pages, fonts, images, annotations, content operators) of a PDF, plus the small public accessors the async layer (plan 02), CLI (plan 04), and TUI (plan 05) build on.

**Architecture:** Everything is additive and pure-sync over the already-loaded byte buffer. Spans come from `Parser::at(data, off)` + `Parser::pos()` after parsing (no lexer changes). Cross-reference section spans come from a new public `parse_section_at` that the existing chain walker is refactored onto. The iterator is an explicit state machine that parses one element per `next()` call.

**Tech Stack:** Rust 2021, pdfboss-core only (memchr is already a core dependency). Zero new dependencies.

## Global Constraints

- Cleanroom rule: implemented purely from ISO 32000. Never name any other PDF library anywhere — code, comments, docs, tests, commit messages.
- `pdfboss-core` gains **zero** new dependencies. No async, no serde, no jq in core.
- The existing sync API and all existing tests stay untouched (refactors must keep every current test green).
- Never create underscore-prefixed identifiers (no `_foo` variables/fields/methods). For unused tuple slots use `..` or positional access, not `_x`.
- Every build/test command uses the shared cargo cache: prefix with `CARGO_TARGET_DIR=$HOME/.cargo/shared-target`.
- After each task: `cargo fmt --all` and `cargo clippy -p pdfboss-core --all-targets -- -D warnings` must pass.
- Spec: `docs/superpowers/specs/2026-07-24-pdf-element-explorer-design.md`. Where this plan and the spec disagree on internals, the plan wins; public API names match the spec.

---

### Task 1: Move `pretty` from the CLI into core

The TUI (plan 05) and CLI both need object pretty-printing; it has no CLI-specific code.

**Files:**
- Create: `crates/pdfboss-core/src/pretty.rs` (moved content)
- Delete: `crates/pdfboss-cli/src/pretty.rs`
- Modify: `crates/pdfboss-core/src/lib.rs`, `crates/pdfboss-cli/src/main.rs:4`

**Interfaces:**
- Consumes: existing `pdfboss_core::object::decode_text_string`, `Dict`, `Name`, `Object`.
- Produces: `pdfboss_core::pretty::format_object(obj: &Object) -> String`, `pdfboss_core::pretty::format_dict(dict: &Dict) -> String` (behavior unchanged).

- [ ] **Step 1: Move the file**

```bash
git mv crates/pdfboss-cli/src/pretty.rs crates/pdfboss-core/src/pretty.rs
```

- [ ] **Step 2: Fix imports for the new crate**

In `crates/pdfboss-core/src/pretty.rs` replace the two import lines

```rust
use pdfboss_core::object::decode_text_string;
use pdfboss_core::{Dict, Name, Object};
```

with

```rust
use crate::object::decode_text_string;
use crate::object::{Dict, Name, Object};
```

and in the `#[cfg(test)] mod tests` block replace `use pdfboss_core::ObjRef;` with `use crate::object::ObjRef;`.

- [ ] **Step 3: Export the module from core**

In `crates/pdfboss-core/src/lib.rs`, add to the module list (alphabetical, after `pub mod parser;`):

```rust
pub mod pretty;
```

- [ ] **Step 4: Point the CLI at the moved module**

In `crates/pdfboss-cli/src/main.rs` replace line 4 `mod pretty;` with:

```rust
use pdfboss_core::pretty;
```

- [ ] **Step 5: Verify everything still builds and passes**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core pretty && CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo build -p pdfboss-cli`
Expected: the moved pretty tests PASS; the CLI builds with no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/pdfboss-core/src/pretty.rs crates/pdfboss-core/src/lib.rs crates/pdfboss-cli/src/main.rs crates/pdfboss-cli/src/pretty.rs
git commit -m "refactor(core): move object pretty-printing from the CLI into pdfboss-core"
```

---

### Task 2: `elements` module scaffold — `Span`, `ElementOpts`, `Element`

**Files:**
- Create: `crates/pdfboss-core/src/elements.rs`
- Modify: `crates/pdfboss-core/src/lib.rs`
- Test: `crates/pdfboss-core/src/elements.rs` (inline `mod tests`)

**Interfaces:**
- Consumes: `crate::object::{Dict, Name, ObjRef, Object}`, `crate::content::Op`, `crate::xref::XrefKind` (added in Task 3 — until then a local placeholder is NOT allowed; instead Task 2 defines `XrefKind` here and Task 3 moves nothing: see Step 2, `XrefKind` lives in `elements.rs` and `xref.rs` imports it).
- Produces (consumed by plans 02–05):
  - `pub struct Span { pub start: u64, pub end: u64 }` + `new`, `len`, `is_empty`
  - `pub enum XrefKind { Table, Stream }`
  - `pub enum Element { … }` exactly as below
  - `pub struct ElementOpts { physical, logical, pages, content_ops }` with `Default` = `(true, true, None, false)`
  - lib.rs re-exports: `pub use elements::{Element, ElementOpts, Span, XrefKind};`

- [ ] **Step 1: Write the failing test**

Create `crates/pdfboss-core/src/elements.rs` containing only the test module for now:

```rust
//! Lazy iteration over a document's elements: the physical file structure
//! (header, indirect objects, cross-reference sections, trailer, startxref,
//! eof) with byte spans, and the logical document structure (pages, fonts,
//! images, annotations, content operators). ISO 32000 §7.5 (file structure)
//! and §7.7 (document structure).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_opts_defaults() {
        let opts = ElementOpts::default();
        assert!(opts.physical);
        assert!(opts.logical);
        assert!(opts.pages.is_none());
        assert!(!opts.content_ops);
    }

    #[test]
    fn span_length_and_emptiness() {
        let span = Span::new(10, 25);
        assert_eq!(span.len(), 15);
        assert!(!span.is_empty());
        assert!(Span::new(7, 7).is_empty());
        assert_eq!(Span::new(9, 3).len(), 0);
    }
}
```

Add to `crates/pdfboss-core/src/lib.rs` module list (alphabetical, after `pub mod document;`):

```rust
pub mod elements;
```

and extend the re-export block at the bottom:

```rust
pub use elements::{Element, ElementOpts, Span, XrefKind};
```

- [ ] **Step 2: Run test to verify it fails**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core elements::tests -- --nocapture`
Expected: COMPILE ERROR — `ElementOpts`, `Span`, `Element`, `XrefKind` not found.

- [ ] **Step 3: Write the definitions**

Insert above the test module in `crates/pdfboss-core/src/elements.rs`:

```rust
use crate::content::Op;
use crate::object::{Dict, Name, ObjRef, Object};

/// Byte range in the physical file, end-exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

impl Span {
    /// A span from `start` (inclusive) to `end` (exclusive).
    pub fn new(start: u64, end: u64) -> Span {
        Span { start, end }
    }

    /// Number of bytes covered; inverted spans count as zero.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Kind of a cross-reference section (ISO 32000 §7.5.4 / §7.5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefKind {
    /// A classic `xref` table.
    Table,
    /// A cross-reference stream.
    Stream,
}

/// One element of a document, physical or logical.
#[derive(Debug, Clone)]
pub enum Element {
    /// The `%PDF-x.y` header.
    Header { version: (u8, u8), span: Span },
    /// One indirect object.
    IndirectObject {
        r: ObjRef,
        object: Object,
        /// Span of `N G obj … endobj` in the file. For objects stored in an
        /// object stream this is the container stream object's span.
        span: Span,
        /// For objects inside an object stream: the container's reference
        /// and this object's byte range within the *decoded* stream data.
        in_objstm: Option<(ObjRef, Span)>,
    },
    /// One cross-reference section (table or stream).
    XrefSection {
        kind: XrefKind,
        span: Span,
        entries: usize,
    },
    /// The trailer: the merged trailer dictionary plus the byte range of the
    /// newest trailer region (classic `trailer << … >>`, or the newest
    /// cross-reference stream object when no classic trailer exists).
    Trailer { dict: Dict, span: Span },
    /// The `startxref` keyword and its offset operand.
    StartXref { offset: u64, span: Span },
    /// The `%%EOF` marker.
    Eof { span: Span },

    /// One page (logical).
    Page { index: usize, r: ObjRef },
    /// One font referenced from a page's resources.
    Font {
        page: Option<usize>,
        r: ObjRef,
        subtype: Name,
        base_font: Option<Name>,
    },
    /// One image XObject referenced from a page's resources.
    Image {
        page: Option<usize>,
        r: ObjRef,
        width: u32,
        height: u32,
    },
    /// One annotation on a page.
    Annotation {
        page: usize,
        r: ObjRef,
        subtype: Name,
    },
    /// One content-stream operator of a page.
    ContentOp {
        page: usize,
        op: Op,
        /// Byte range within the page's decoded, concatenated content.
        span_in_content: Span,
    },
}

/// Selects which element layers [`crate::Document::elements`] yields.
#[derive(Debug, Clone)]
pub struct ElementOpts {
    /// Yield physical file-structure elements.
    pub physical: bool,
    /// Yield logical document-structure elements.
    pub logical: bool,
    /// Restrict logical elements to these 0-based page indices.
    pub pages: Option<Vec<usize>>,
    /// Yield [`Element::ContentOp`] items (high-volume; off by default).
    pub content_ops: bool,
}

impl Default for ElementOpts {
    fn default() -> Self {
        ElementOpts {
            physical: true,
            logical: true,
            pages: None,
            content_ops: false,
        }
    }
}
```

The `use crate::content::Op;` and `use crate::object::…` imports are used by `Element`; `Dict` and `Object` appear in variants added here, so no unused-import warnings.

- [ ] **Step 4: Run test to verify it passes**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core elements::tests -- --nocapture`
Expected: both tests PASS. `cargo clippy -p pdfboss-core --all-targets -- -D warnings` clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pdfboss-core/src/elements.rs crates/pdfboss-core/src/lib.rs
git commit -m "feat(core): element model types (Span, Element, ElementOpts, XrefKind)"
```

---

### Task 3: Public xref iteration and section parsing with spans

`Elements` (Task 7) and `pdfboss-aio` (plan 02) need (a) all xref entries and (b) per-section parsing that reports byte extents and chain pointers.

**Files:**
- Modify: `crates/pdfboss-core/src/xref.rs`
- Test: `crates/pdfboss-core/src/xref.rs` (inline `mod tests` — this file already has tests at the bottom; add to them)

**Interfaces:**
- Consumes: `crate::elements::{Span, XrefKind}` (Task 2), existing private `parse_classic` / `parse_stream_section` / `load_chain`.
- Produces (consumed by Task 7 and plan 02):
  - `impl Xref { pub fn iter(&self) -> impl Iterator<Item = (u32, XrefEntry)> + '_; pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool; }`
  - `pub struct XrefSectionInfo { pub xref: Xref, pub kind: XrefKind, pub prev: Option<i64>, pub xrefstm: Option<i64>, pub span: Span, pub trailer_span: Option<Span> }`
  - `pub fn parse_section_at(data: &[u8], off: usize) -> Result<XrefSectionInfo>`

- [ ] **Step 1: Write the failing tests**

Append inside the existing `#[cfg(test)] mod tests` in `crates/pdfboss-core/src/xref.rs`:

```rust
    #[test]
    fn iter_reports_every_entry() {
        let data = pdfboss_testkit::simple_doc("iter");
        let xref = load_xref(&data).unwrap();
        let mut nums: Vec<u32> = xref.iter().map(|pair| pair.0).collect();
        nums.sort_unstable();
        assert_eq!(nums.len(), xref.len());
        assert!(!xref.is_empty());
        for (num, entry) in xref.iter() {
            assert_eq!(xref.get(num), Some(entry));
        }
    }

    #[test]
    fn parse_section_at_classic_reports_spans() {
        let data = pdfboss_testkit::simple_doc("spans");
        let off = memchr::memmem::rfind(&data, b"xref").unwrap();
        let info = parse_section_at(&data, off).unwrap();
        assert_eq!(info.kind, crate::elements::XrefKind::Table);
        assert!(info.prev.is_none());
        assert!(info.xrefstm.is_none());
        assert!(!info.xref.is_empty());
        // The section span starts at `xref` and ends where the trailer begins.
        assert_eq!(info.span.start, off as u64);
        let tspan = info.trailer_span.expect("classic sections have a trailer");
        assert_eq!(info.span.end, tspan.start);
        assert!(data[tspan.start as usize..].starts_with(b"trailer"));
        // Re-parsing the trailer region yields the same dictionary.
        let mut parser = Parser::at(&data, tspan.start as usize + b"trailer".len());
        let reparsed = parser.parse_object(&NoResolve).unwrap();
        assert_eq!(reparsed.as_dict().unwrap().get("Root"), info.xref.trailer.get("Root"));
        assert!(tspan.end as usize <= data.len());
    }

    #[test]
    fn parse_section_at_stream_reports_spans() {
        let mut builder = pdfboss_testkit::PdfBuilder::new();
        builder.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        builder.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        builder.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
        let data = builder.build_xref_stream(1);
        let startxref = memchr::memmem::rfind(&data, b"startxref").unwrap();
        let mut lexer = Lexer::at(&data, startxref + b"startxref".len());
        let off = match lexer.next_token().unwrap() {
            Token::Int(v) => v as usize,
            other => panic!("expected startxref offset, got {other:?}"),
        };
        let info = parse_section_at(&data, off).unwrap();
        assert_eq!(info.kind, crate::elements::XrefKind::Stream);
        assert!(info.trailer_span.is_none());
        assert_eq!(info.span.start, off as u64);
        assert!(info.span.end > info.span.start && info.span.end as u64 <= data.len() as u64);
        // The span covers the whole stream object.
        let body = &data[info.span.start as usize..info.span.end as usize];
        assert!(memchr::memmem::find(body, b"endstream").is_some());
    }
```

(`pdfboss_testkit` is already a dev-dependency of pdfboss-core — the document tests use it.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core xref::tests::iter_reports -- --nocapture`
Expected: COMPILE ERROR — no method `iter`, no `parse_section_at`, no `XrefSectionInfo`.

- [ ] **Step 3: Implement**

In `crates/pdfboss-core/src/xref.rs`:

(a) Add the import at the top (after the existing `use crate::parser::…` line):

```rust
use crate::elements::{Span, XrefKind};
```

(b) Add to `impl Xref` (after `merge`):

```rust
    /// Iterates all `(object number, entry)` pairs in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, XrefEntry)> + '_ {
        self.map.iter().map(|(&num, &entry)| (num, entry))
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the table has no entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
```

(c) Add the public section type and parser (place directly above `parse_classic`):

```rust
/// One parsed cross-reference section with its byte extents, for element
/// iteration and for chain walkers that fetch byte ranges on demand.
#[derive(Debug, Clone)]
pub struct XrefSectionInfo {
    /// This section's entries plus its trailer keys.
    pub xref: Xref,
    pub kind: XrefKind,
    /// The trailer's `/Prev` value, when present.
    pub prev: Option<i64>,
    /// The classic trailer's `/XRefStm` value (hybrid files), when present.
    pub xrefstm: Option<i64>,
    /// Byte range of the section itself: the `xref` table (excluding its
    /// trailer) or the whole cross-reference stream object.
    pub span: Span,
    /// Classic sections: byte range of `trailer << … >>`. Stream sections
    /// have no separate trailer region.
    pub trailer_span: Option<Span>,
}

/// Parses the cross-reference section at `off` — a classic table or a
/// cross-reference stream — reporting entries, chain pointers, and spans.
pub fn parse_section_at(data: &[u8], off: usize) -> Result<XrefSectionInfo> {
    let mut lexer = Lexer::at(data, off);
    if matches!(lexer.peek_token(), Ok(Token::Keyword(ref k)) if k.as_slice() == b"xref") {
        parse_classic(data, off)
    } else {
        parse_stream_section(data, off)
    }
}
```

(d) Change `parse_classic` to return `XrefSectionInfo`. Full replacement (the entry-reading loop is unchanged; only the signature and the `trailer` arm change):

```rust
/// Parses a classic `xref` section at `off`: `start count` subsection
/// headers, then `count` entries each of `offset gen n|f`, ending with
/// `trailer` and its dictionary. Entries are read token-wise, so malformed
/// 19- or 21-byte entry lines load just as well as conforming 20-byte ones.
fn parse_classic(data: &[u8], off: usize) -> Result<XrefSectionInfo> {
    let mut lexer = Lexer::at(data, off);
    match lexer.next_token()? {
        Token::Keyword(ref k) if k.as_slice() == b"xref" => {}
        _ => return Err(Error::InvalidXref),
    }
    let mut section = Xref::default();
    loop {
        let before = lexer.pos();
        match lexer.next_token()? {
            Token::Int(start) if start >= 0 => {
                let count = match lexer.next_token()? {
                    Token::Int(c) if c >= 0 => c as u64,
                    _ => return Err(Error::InvalidXref),
                };
                // Even a degenerate entry line needs at least 11 bytes, so
                // a count beyond this bound cannot be real.
                if count > data.len() as u64 / 11 + 1 {
                    return Err(Error::InvalidXref);
                }
                for i in 0..count {
                    let f1 = match lexer.next_token()? {
                        Token::Int(v) if v >= 0 => v as u64,
                        _ => return Err(Error::InvalidXref),
                    };
                    let f2 = match lexer.next_token()? {
                        Token::Int(v) if v >= 0 => v,
                        _ => return Err(Error::InvalidXref),
                    };
                    let entry = match lexer.next_token()? {
                        Token::Keyword(ref k) if k.as_slice() == b"n" => XrefEntry::InFile {
                            offset: f1,
                            gen: f2.min(65535) as u16,
                        },
                        Token::Keyword(ref k) if k.as_slice() == b"f" => XrefEntry::Free,
                        _ => return Err(Error::InvalidXref),
                    };
                    if let Ok(num) = u32::try_from(start as u64 + i) {
                        section.add(num, entry);
                    }
                }
            }
            Token::Keyword(ref k) if k.as_slice() == b"trailer" => {
                // `before` points at (or at whitespace just before) the
                // `trailer` keyword; the keyword itself ends at lexer.pos().
                let trailer_start = lexer.pos() - b"trailer".len();
                let mut parser = Parser::at(data, lexer.pos());
                let trailer = match parser.parse_object(&NoResolve)? {
                    Object::Dict(d) => d,
                    _ => return Err(Error::InvalidXref),
                };
                let prev = trailer.get_int("Prev");
                let xrefstm = trailer.get_int("XRefStm");
                section.trailer = trailer;
                return Ok(XrefSectionInfo {
                    xref: section,
                    kind: XrefKind::Table,
                    prev,
                    xrefstm,
                    span: Span::new(off as u64, before.min(trailer_start) as u64),
                    trailer_span: Some(Span::new(trailer_start as u64, parser.pos() as u64)),
                });
            }
            _ => return Err(Error::InvalidXref),
        }
    }
}
```

Note the added `let before = lexer.pos();` at the top of the loop: it records the position before the token that turned out to be `trailer`, so the table span ends before any whitespace that precedes the keyword — `before.min(trailer_start)` keeps the tighter bound.

(e) Change `parse_stream_section` the same way — only the signature, one added line, and the return change. Replace the signature and the final three lines:

```rust
fn parse_stream_section(data: &[u8], off: usize) -> Result<XrefSectionInfo> {
    let mut parser = Parser::at(data, off);
    let (_, obj) = parser.parse_indirect(&NoResolve)?;
    let end = parser.pos();
```

(the body between stays byte-for-byte as today) and at the end:

```rust
    let prev = dict.get_int("Prev");
    section.trailer = dict;
    Ok(XrefSectionInfo {
        xref: section,
        kind: XrefKind::Stream,
        prev,
        xrefstm: None,
        span: Span::new(off as u64, end as u64),
        trailer_span: None,
    })
}
```

Note: `parse_indirect` destructures with `(_, obj)` — that underscore is a bare wildcard pattern, not an identifier, and is allowed (it already exists in this function today).

(f) Refactor `load_chain` onto the new return type. Full replacement of the `while` body's section handling:

```rust
fn load_chain(data: &[u8], start: usize) -> Result<Xref> {
    let mut acc = Xref::default();
    let mut visited: FastSet<usize> = FastSet::default();
    let mut next = Some(start);
    while let Some(off) = next {
        if !visited.insert(off) {
            break;
        }
        let info = parse_section_at(data, off)?;
        if let Some(xs) = info.xrefstm.and_then(|v| to_offset(v, data)) {
            if visited.insert(xs) {
                // Lenient: a broken hybrid stream leaves the table alone.
                if let Ok(stream_info) = parse_section_at(data, xs) {
                    acc.merge(stream_info.xref);
                }
            }
        }
        acc.merge(info.xref);
        next = info.prev.and_then(|v| to_offset(v, data));
    }
    if acc.map.is_empty() {
        Err(Error::InvalidXref)
    } else {
        Ok(acc)
    }
}
```

(The classic/stream dispatch that `load_chain` used to do inline now lives in `parse_section_at`; hybrid `/XRefStm` handling is preserved because stream sections return `xrefstm: None`, so only classic sections trigger the hybrid merge — same as before.)

- [ ] **Step 4: Run the whole core suite to verify new tests pass and nothing regressed**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core`
Expected: all tests PASS, including the three new ones. Then `cargo clippy -p pdfboss-core --all-targets -- -D warnings` clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pdfboss-core/src/xref.rs
git commit -m "feat(core): public xref iteration and span-reporting section parser"
```

---

### Task 4: Document accessors — raw bytes, xref, spanned object parse, objstm handle, page refs

**Files:**
- Modify: `crates/pdfboss-core/src/document.rs`, `crates/pdfboss-core/src/objstm.rs`
- Test: inline `mod tests` in both files

**Interfaces:**
- Consumes: `crate::elements::Span` (Task 2).
- Produces (consumed by Tasks 7–9 and plans 02/04/05):
  - `Document::bytes(&self) -> &[u8]` (pub)
  - `Document::xref(&self) -> &Xref` (pub)
  - `Document::object_at_spanned(&self, offset: usize) -> Result<(ObjRef, Object, Span)>` (pub(crate))
  - `Document::objstm_handle(&self, stream_num: u32) -> Result<Rc<objstm::ObjStm>>` (pub(crate))
  - `ObjStm::object_spanned(&self, index: u32) -> Result<(Object, (usize, usize))>` (pub)
  - `Page::object_ref(&self) -> Option<ObjRef>` (pub)

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` in `crates/pdfboss-core/src/document.rs`:

```rust
    #[test]
    fn bytes_and_xref_accessors() {
        let data = simple_doc("accessors");
        let doc = Document::load(data.clone()).unwrap();
        assert_eq!(doc.bytes(), &data[..]);
        assert!(doc.xref().len() > 0);
        assert!(doc.xref().trailer.get("Root").is_some());
    }

    #[test]
    fn object_at_spanned_reparses_identically() {
        let data = simple_doc("spanned");
        let doc = Document::load(data).unwrap();
        for (num, entry) in doc.xref().iter() {
            let XrefEntry::InFile { offset, gen } = entry else {
                continue;
            };
            let (r, object, span) = doc.object_at_spanned(offset as usize).unwrap();
            assert_eq!(r.num, num);
            assert_eq!(r.gen, gen);
            assert_eq!(span.start, offset);
            assert!(span.end as usize <= doc.bytes().len());
            // The bytes at the span parse back to the same object.
            let slice = &doc.bytes()[span.start as usize..span.end as usize];
            let (r2, object2) = Parser::new(slice).parse_indirect(&NoResolve).unwrap();
            assert_eq!(r2, r);
            assert_eq!(object2, object);
        }
    }

    #[test]
    fn page_object_ref_points_at_a_page_dict() {
        let doc = Document::load(multi_page_doc(&["one", "two"])).unwrap();
        for index in 0..doc.page_count() {
            let page = doc.page(index).unwrap();
            let r = page.object_ref().expect("builder pages are indirect");
            let resolved = doc.get(r).unwrap();
            assert_eq!(
                resolved.as_dict().unwrap().get_name("Type").map(|n| n.0.as_str()),
                Some("Page")
            );
        }
    }
```

Add the needed test imports to the existing `use` lines in that test module:

```rust
    use crate::parser::{NoResolve, Parser};
    use crate::xref::XrefEntry;
```

Append to the `#[cfg(test)] mod tests` in `crates/pdfboss-core/src/objstm.rs`:

```rust
    #[test]
    fn object_spanned_reports_reparseable_range() {
        let (data, n, first) = build_stream(&[(11, "<< /A 1 >>"), (12, "(hi)")]);
        let stm = ObjStm::parse(data.clone(), n, first).unwrap();
        for index in 0..2u32 {
            let (object, (start, end)) = stm.object_spanned(index).unwrap();
            assert!(start >= first && end <= data.len() && start < end);
            let reparsed = Parser::at(&data, start).parse_object(&NoResolve).unwrap();
            assert_eq!(reparsed, object);
            assert_eq!(stm.object(index).unwrap(), object);
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core object_at_spanned -- --nocapture`
Expected: COMPILE ERROR — no `bytes`, `object_at_spanned`, `object_ref`, `object_spanned`.

- [ ] **Step 3: Implement**

In `crates/pdfboss-core/src/objstm.rs`, replace `ObjStm::object` with a spanned core plus a delegating wrapper:

```rust
    /// Parses the object at `index` from the already-decoded bytes.
    pub fn object(&self, index: u32) -> Result<Object> {
        self.object_spanned(index).map(|pair| pair.0)
    }

    /// Parses the object at `index`, also reporting its byte range within
    /// the decoded stream data.
    pub fn object_spanned(&self, index: u32) -> Result<(Object, (usize, usize))> {
        let offset = *self.offsets.get(index as usize).ok_or_else(|| {
            Error::Other(format!(
                "object stream index {index} out of range (N = {})",
                self.offsets.len()
            ))
        })?;
        let pos = self
            .first
            .checked_add(offset)
            .filter(|&p| p <= self.data.len())
            .ok_or_else(|| {
                Error::Other(format!(
                    "object stream offset {offset} lies outside the stream"
                ))
            })?;
        let mut parser = Parser::at(&self.data, pos);
        let object = parser.parse_object(&NoResolve)?;
        Ok((object, (pos, parser.pos())))
    }
```

In `crates/pdfboss-core/src/document.rs`:

(a) Import `Span` (extend the existing crate imports near the top):

```rust
use crate::elements::Span;
```

(b) Add public accessors to `impl Document` (after `version`):

```rust
    /// Raw bytes of the loaded file.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// The merged cross-reference table and trailer.
    pub fn xref(&self) -> &Xref {
        &self.xref
    }
```

(c) Add the spanned parse helper and refactor `load_object`'s `InFile` arm onto it:

```rust
    /// Parses the indirect object at `offset`, applying decryption, and
    /// reports the byte range consumed (`N G obj … endobj`).
    pub(crate) fn object_at_spanned(&self, offset: usize) -> Result<(ObjRef, Object, Span)> {
        let mut parser = Parser::at(&self.data, offset);
        let (r, mut object) = parser.parse_indirect(self)?;
        // Objects stored directly in the file carry encrypted strings and
        // stream data; decrypt with this object's key. (Objects living in
        // object streams are decrypted with their container.)
        if let Some(dec) = &self.decryptor {
            dec.decrypt_object(&mut object, r.num, r.gen);
        }
        Ok((r, object, Span::new(offset as u64, parser.pos() as u64)))
    }
```

and in `load_object`, replace the `InFile` arm's body with:

```rust
            Some(XrefEntry::InFile { offset, .. }) => {
                let offset = usize::try_from(offset)
                    .ok()
                    .filter(|&o| o < self.data.len())
                    .ok_or(Error::ObjectNotFound(r.num, r.gen))?;
                self.object_at_spanned(offset).map(|parsed| parsed.1)
            }
```

(d) Add the object-stream handle helper and refactor `load_from_object_stream` onto it:

```rust
    /// The decoded, header-parsed object stream `stream_num`, built at most
    /// once and cached.
    pub(crate) fn objstm_handle(&self, stream_num: u32) -> Result<Rc<objstm::ObjStm>> {
        if let Some(stm) = self.objstms.borrow().get(&stream_num) {
            return Ok(Rc::clone(stm));
        }
        let container = self.get(ObjRef {
            num: stream_num,
            gen: 0,
        })?;
        let stream = container.as_stream().ok_or_else(|| Error::TypeMismatch {
            expected: "stream",
            found: type_name(&container),
        })?;
        let n = self
            .resolve(stream.dict.get("N").unwrap_or(&Object::Null))?
            .as_int()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(Error::MissingKey("N"))?;
        let first = self
            .resolve(stream.dict.get("First").unwrap_or(&Object::Null))?
            .as_int()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(Error::MissingKey("First"))?;
        let decoded = self.stream_data(stream)?;
        let stm = Rc::new(objstm::ObjStm::parse(decoded, n, first)?);
        self.objstms.borrow_mut().insert(stream_num, Rc::clone(&stm));
        Ok(stm)
    }

    /// Extracts a compressed object from the object stream `stream_num`.
    fn load_from_object_stream(&self, stream_num: u32, index: u32) -> Result<Object> {
        self.objstm_handle(stream_num)?.object(index)
    }
```

(these two replace the current `load_from_object_stream` entirely).

(e) Track each page's object reference. Change `PageRec`:

```rust
/// The flattened, inheritance-applied record for one page.
struct PageRec {
    obj_ref: Option<ObjRef>,
    media_box: Rect,
    crop_box: Rect,
    rotate: i32,
    resources: Dict,
    dict: Dict,
}
```

In `flatten_pages`, capture the ref before resolving (the `if let` that guards cycles already binds it — reuse the same spot). Replace:

```rust
            if let Object::Ref(r) = node {
                if !visited.insert(r) {
                    continue; // cycle: this node was already traversed
                }
            }
```

with:

```rust
            let node_ref = if let Object::Ref(r) = node { Some(r) } else { None };
            if let Some(r) = node_ref {
                if !visited.insert(r) {
                    continue; // cycle: this node was already traversed
                }
            }
```

and change the leaf push `None => pages.push(make_page_rec(dict.clone(), &inherited)),` to:

```rust
                None => pages.push(make_page_rec(node_ref, dict.clone(), &inherited)),
```

Update `make_page_rec`:

```rust
/// Builds the final page record from a leaf dictionary and its inherited
/// attributes, applying the spec defaults.
fn make_page_rec(obj_ref: Option<ObjRef>, dict: Dict, inherited: &Inherited) -> PageRec {
    let media_box = inherited
        .media_box
        .filter(|r| r.width() > 0.0 && r.height() > 0.0)
        .unwrap_or(US_LETTER);
    let crop_box = inherited
        .crop_box
        .and_then(|c| c.intersect(media_box))
        .filter(|r| r.width() > 0.0 && r.height() > 0.0)
        .unwrap_or(media_box);
    PageRec {
        obj_ref,
        media_box,
        crop_box,
        rotate: normalize_rotation(inherited.rotate.unwrap_or(0)),
        resources: inherited.resources.clone().unwrap_or_default(),
        dict,
    }
}
```

Extend `Page` with the ref (private field + accessor, keeping existing public fields as-is):

```rust
pub struct Page {
    /// 0-based page index.
    pub index: usize,
    pub media_box: Rect,
    pub crop_box: Rect,
    pub rotate: i32,
    /// The page's (inherited) `/Resources` dictionary.
    pub resources: Dict,
    dict: Dict,
    obj_ref: Option<ObjRef>,
}
```

add to `impl Page`:

```rust
    /// The page's indirect object reference, when the page came from an
    /// indirect kid in the page tree (pages inlined directly into a `/Kids`
    /// array have none).
    pub fn object_ref(&self) -> Option<ObjRef> {
        self.obj_ref
    }
```

and in `Document::page`, add the field to the constructor:

```rust
        Ok(Page {
            index,
            media_box: rec.media_box,
            crop_box: rec.crop_box,
            rotate: rec.rotate,
            resources: rec.resources.clone(),
            dict: rec.dict.clone(),
            obj_ref: rec.obj_ref,
        })
```

- [ ] **Step 4: Run the whole core suite**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core`
Expected: all tests PASS (new and existing). Clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pdfboss-core/src/document.rs crates/pdfboss-core/src/objstm.rs
git commit -m "feat(core): document byte/xref accessors, spanned parses, page object refs"
```

---

### Task 5: Spanned content-stream parsing

**Files:**
- Modify: `crates/pdfboss-core/src/content.rs`
- Test: inline `mod tests` in `content.rs`

**Interfaces:**
- Consumes: `crate::elements::Span` (Task 2), existing `Lexer::skip_whitespace_and_comments` / `Lexer::pos`.
- Produces (consumed by Task 9, plan 05):
  - `pub fn parse_content_spanned(data: &[u8]) -> Result<Vec<(Op, Span)>>`
  - `parse_content` unchanged in behavior (delegates).

- [ ] **Step 1: Write the failing test**

Append to `content.rs` tests:

```rust
    #[test]
    fn spanned_ops_cover_their_source_bytes() {
        let data = b"q 1 0 0 1 5 5 cm BT /F1 12 Tf (Hi) Tj ET Q";
        let spanned = parse_content_spanned(data).unwrap();
        let plain = parse_content(data).unwrap();
        assert_eq!(
            spanned.iter().map(|pair| pair.0.clone()).collect::<Vec<_>>(),
            plain
        );
        for (op, span) in &spanned {
            assert!(span.start < span.end && span.end as usize <= data.len());
            // Re-parsing exactly the spanned bytes yields exactly this op.
            let slice = &data[span.start as usize..span.end as usize];
            let reparsed = parse_content(slice).unwrap();
            assert_eq!(reparsed.len(), 1, "span of {op:?} reparses to one op");
            assert_eq!(&reparsed[0], op);
        }
        // Spans are ordered and non-overlapping.
        for pair in spanned.windows(2) {
            assert!(pair[0].1.end <= pair[1].1.start);
        }
    }

    #[test]
    fn unknown_operator_does_not_stretch_the_next_span() {
        // `zz` is unknown: its operands are dropped, and the following op's
        // span must start at `q`, not at `7`.
        let data = b"7 8 zz q";
        let spanned = parse_content_spanned(data).unwrap();
        assert_eq!(spanned.len(), 1);
        assert_eq!(spanned[0].0, Op::Save);
        assert_eq!(&data[spanned[0].1.start as usize..spanned[0].1.end as usize], b"q");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core content::tests::spanned -- --nocapture`
Expected: COMPILE ERROR — `parse_content_spanned` not found.

- [ ] **Step 3: Implement**

In `content.rs`, add the import:

```rust
use crate::elements::Span;
```

Replace `parse_content` with the pair:

```rust
/// Parses a decoded content stream into a sequence of operators. Inline
/// image data runs from after `ID` plus one whitespace byte to `EI` at a
/// token boundary (or the declared `/L`ength when present, which is
/// trusted).
pub fn parse_content(data: &[u8]) -> Result<Vec<Op>> {
    Ok(parse_content_spanned(data)?
        .into_iter()
        .map(|pair| pair.0)
        .collect())
}

/// Like [`parse_content`], but also reports each operator's byte range —
/// from the first token of its operand run through the operator keyword —
/// within `data`.
pub fn parse_content_spanned(data: &[u8]) -> Result<Vec<(Op, Span)>> {
    let mut lexer = Lexer::new(data);
    let mut ops = Vec::new();
    let mut stack: Vec<Object> = Vec::new();
    // Start of the current operand run; cleared whenever the run dies
    // (an operator was emitted, an unknown operator dropped it, or a
    // stray closer flushed it).
    let mut run_start: Option<usize> = None;
    loop {
        lexer.skip_whitespace_and_comments();
        let token_start = lexer.pos();
        let token = lexer.next_token()?;
        if !matches!(token, Token::Eof) && run_start.is_none() {
            run_start = Some(token_start);
        }
        match token {
            Token::Eof => break,
            Token::Int(i) => stack.push(Object::Int(i)),
            Token::Real(r) => stack.push(Object::Real(r)),
            Token::Name(n) => stack.push(Object::Name(n)),
            Token::LitString(s) | Token::HexString(s) => stack.push(Object::String(s)),
            Token::ArrayOpen => {
                let a = parse_array(&mut lexer, 0)?;
                stack.push(a);
            }
            Token::DictOpen => {
                let d = parse_dict(&mut lexer, 0)?;
                stack.push(Object::Dict(d));
            }
            // Stray closers: malformed input, drop pending operands.
            Token::ArrayClose | Token::DictClose => {
                stack.clear();
                run_start = None;
            }
            Token::Keyword(kw) => match kw.as_slice() {
                b"true" => stack.push(Object::Bool(true)),
                b"false" => stack.push(Object::Bool(false)),
                b"null" => stack.push(Object::Null),
                b"BI" => {
                    let start = run_start.take().unwrap_or(token_start);
                    if let Some(op) = parse_inline_image(&mut lexer) {
                        ops.push((op, Span::new(start as u64, lexer.pos() as u64)));
                    }
                    stack.clear();
                }
                _ => {
                    let start = run_start.take().unwrap_or(token_start);
                    if let Some(op) = dispatch(&kw, &stack) {
                        ops.push((op, Span::new(start as u64, lexer.pos() as u64)));
                    }
                    stack.clear();
                }
            },
        }
    }
    Ok(ops)
}
```

Behavioral notes baked into the tests: `run_start.take()` clears the run whether or not `dispatch` recognized the keyword, so unknown operators drop their operands *and* their span (second test). `true`/`false`/`null` are operands and keep the run open.

- [ ] **Step 4: Run the whole core suite**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core`
Expected: all tests PASS — the existing content tests exercise `parse_content`, which now delegates, so any drift shows up here. Clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pdfboss-core/src/content.rs
git commit -m "feat(core): span-reporting content-stream parser"
```

---

### Task 6: Testkit fixture — a document with objects in an object stream

Element tests (Task 7) and plan 02's parity tests need a well-formed file whose objects live in an object stream referenced by a cross-reference stream. The testkit has `objstm_payload` (payload only); add a whole-file fixture.

**Files:**
- Modify: `crates/pdfboss-testkit/src/lib.rs`
- Test: `crates/pdfboss-core/src/document.rs` (inline; proves the fixture loads)

**Interfaces:**
- Consumes: existing testkit internals (`PdfBuilder` layout conventions — read `build_xref_stream` at `crates/pdfboss-testkit/src/lib.rs:126` before writing, and reuse its xref-stream emission helpers if they are separable; otherwise emit bytes directly as below).
- Produces: `pub fn objstm_doc(extra: &[(u32, &str)]) -> Vec<u8>` — a one-page document whose catalog, page tree, and page objects (1, 2, 3) plus every `extra` object live inside object stream 4, indexed by cross-reference stream 5.

- [ ] **Step 1: Write the failing test**

Append to `document.rs` tests:

```rust
    #[test]
    fn objstm_doc_fixture_loads_and_resolves_members() {
        let data = pdfboss_testkit::objstm_doc(&[(7, "<< /Marker (inside) >>")]);
        let doc = Document::load(data).unwrap();
        assert_eq!(doc.page_count(), 1);
        let member = doc.get(ObjRef { num: 7, gen: 0 }).unwrap();
        let text = member.as_dict().unwrap().get("Marker").unwrap();
        assert_eq!(text.as_str_bytes(), Some(&b"inside"[..]));
        // The member really is xref'd into the object stream.
        assert!(matches!(
            doc.xref().get(7),
            Some(XrefEntry::InStream { stream_num: 4, .. })
        ));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core objstm_doc_fixture -- --nocapture`
Expected: COMPILE ERROR — `objstm_doc` not found in `pdfboss_testkit`.

- [ ] **Step 3: Implement the fixture**

Append to `crates/pdfboss-testkit/src/lib.rs`:

```rust
/// A complete one-page document whose catalog (1), page tree (2), and page
/// (3) — plus every `(num, body)` pair in `extra` — live inside object
/// stream 4, indexed by cross-reference stream 5. Object numbers 1–5 are
/// reserved; `extra` numbers must be ≥ 6.
pub fn objstm_doc(extra: &[(u32, &str)]) -> Vec<u8> {
    let mut members: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
    ];
    for (num, body) in extra {
        assert!(*num >= 6, "extra object numbers must be >= 6");
        members.push((*num, (*body).to_string()));
    }

    // Object-stream payload: header of `num offset` pairs, then the bodies.
    let mut header = String::new();
    let mut bodies = String::new();
    for (num, body) in &members {
        header.push_str(&format!("{} {} ", num, bodies.len()));
        bodies.push_str(body);
        bodies.push(' ');
    }
    let first = header.len();
    let payload = format!("{header}{bodies}");

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n");
    let objstm_offset = out.len();
    out.extend_from_slice(
        format!(
            "4 0 obj << /Type /ObjStm /N {} /First {} /Length {} >> stream\n",
            members.len(),
            first,
            payload.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(payload.as_bytes());
    out.extend_from_slice(b"\nendstream endobj\n");
    let xref_offset = out.len();

    // Cross-reference stream 5: W [1 2 1]; entries for objects 0..=max.
    let max_num = members.iter().map(|m| m.0).max().unwrap_or(5).max(5);
    let mut entries: Vec<u8> = Vec::new();
    for num in 0..=max_num {
        if num == 0 {
            entries.extend_from_slice(&[0, 0, 0, 255]); // free head
        } else if num == 4 {
            entries.push(1); // in file
            entries.extend_from_slice(&(objstm_offset as u16).to_be_bytes());
            entries.push(0);
        } else if num == 5 {
            entries.push(1); // the xref stream itself
            entries.extend_from_slice(&(xref_offset as u16).to_be_bytes());
            entries.push(0);
        } else if let Some(index) = members.iter().position(|m| m.0 == num) {
            entries.push(2); // in object stream 4
            entries.extend_from_slice(&4u16.to_be_bytes());
            entries.push(index as u8);
        } else {
            entries.extend_from_slice(&[0, 0, 0, 0]); // free gap
        }
    }
    out.extend_from_slice(
        format!(
            "5 0 obj << /Type /XRef /Size {} /W [1 2 1] /Root 1 0 R /Length {} >> stream\n",
            max_num + 1,
            entries.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&entries);
    out.extend_from_slice(b"\nendstream endobj\n");
    out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    out
}
```

Constraints this encodes: offsets are emitted as 2-byte big-endian (`u16`), which holds because the fixture is tiny; the xref stream is uncompressed (no `/Filter`), which the loader accepts; member index fits `u8` for the same reason. The `assert!` guards misuse from future tests.

- [ ] **Step 4: Run test to verify it passes**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core objstm_doc_fixture -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pdfboss-testkit/src/lib.rs crates/pdfboss-core/src/document.rs
git commit -m "test(testkit): whole-file object-stream fixture"
```

---

### Task 7: `Elements` iterator — physical layer

**Files:**
- Modify: `crates/pdfboss-core/src/elements.rs`
- Test: inline `mod tests` in `elements.rs`

**Interfaces:**
- Consumes: Tasks 2–6 (`Span`, `Element`, `ElementOpts`, `XrefKind`, `Xref::iter`, `parse_section_at`, `XrefSectionInfo`, `Document::{bytes, xref, object_at_spanned, objstm_handle}`, `ObjStm::object_spanned`, testkit `objstm_doc`).
- Produces (spec-pinned; consumed by Tasks 8–9 and plans 02/04/05):
  - `Document::elements(&self, opts: ElementOpts) -> Elements<'_>`
  - `pub struct Elements<'a>` with `impl Iterator<Item = Result<Element>>`
  - Ordering: Header → objects in file order (object-stream members directly after their container, by member index) → xref sections newest→oldest → Trailer → StartXref → Eof → logical layers (Task 8).
  - Salvage: an object that fails to parse yields `Err` for that item; iteration continues.

- [ ] **Step 1: Write the failing tests**

Append to `elements.rs` tests (inside the existing `mod tests`):

```rust
    use crate::document::Document;
    use crate::error::Result;
    use crate::object::ObjRef;
    use crate::parser::{NoResolve, Parser};

    fn physical(doc: &Document) -> Vec<Element> {
        let opts = ElementOpts {
            logical: false,
            ..ElementOpts::default()
        };
        doc.elements(opts).collect::<Result<Vec<_>>>().unwrap()
    }

    #[test]
    fn simple_doc_physical_walk() {
        let data = pdfboss_testkit::simple_doc("walk");
        let doc = Document::load(data).unwrap();
        let elements = physical(&doc);

        let Element::Header { version, span } = &elements[0] else {
            panic!("first element must be the header, got {:?}", elements[0]);
        };
        assert_eq!(*version, (1, 7));
        assert!(doc.bytes()[span.start as usize..].starts_with(b"%PDF-1.7"));

        let mut object_count = 0usize;
        let mut previous_end = 0u64;
        for element in &elements {
            if let Element::IndirectObject { r, object, span, in_objstm } = element {
                assert!(in_objstm.is_none());
                assert!(span.start >= previous_end, "objects come in file order");
                previous_end = span.end;
                let slice = &doc.bytes()[span.start as usize..span.end as usize];
                let (r2, object2) = Parser::new(slice).parse_indirect(&NoResolve).unwrap();
                assert_eq!(r2, *r);
                assert_eq!(object2, *object);
                object_count += 1;
            }
        }
        assert_eq!(object_count, doc.xref().len());

        // Exactly one of each closing element, in order, after the objects.
        let tail_kinds: Vec<&str> = elements
            .iter()
            .filter_map(|e| match e {
                Element::XrefSection { .. } => Some("xref"),
                Element::Trailer { .. } => Some("trailer"),
                Element::StartXref { .. } => Some("startxref"),
                Element::Eof { .. } => Some("eof"),
                _ => None,
            })
            .collect();
        assert_eq!(tail_kinds, ["xref", "trailer", "startxref", "eof"]);

        for element in &elements {
            match element {
                Element::XrefSection { kind, span, entries } => {
                    assert_eq!(*kind, XrefKind::Table);
                    assert!(*entries > 0);
                    assert!(doc.bytes()[span.start as usize..].starts_with(b"xref"));
                }
                Element::Trailer { dict, span } => {
                    assert!(dict.get("Root").is_some());
                    assert!(doc.bytes()[span.start as usize..].starts_with(b"trailer"));
                }
                Element::StartXref { offset, span } => {
                    assert!(doc.bytes()[span.start as usize..].starts_with(b"startxref"));
                    assert!(*offset > 0);
                }
                Element::Eof { span } => {
                    assert!(doc.bytes()[span.start as usize..].starts_with(b"%%EOF"));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn objstm_members_follow_their_container() {
        let data = pdfboss_testkit::objstm_doc(&[(7, "(seven)"), (8, "(eight)")]);
        let doc = Document::load(data).unwrap();
        let elements = physical(&doc);
        let order: Vec<(u32, bool)> = elements
            .iter()
            .filter_map(|e| match e {
                Element::IndirectObject { r, in_objstm, .. } => {
                    Some((r.num, in_objstm.is_some()))
                }
                _ => None,
            })
            .collect();
        // Container 4 comes first (lowest offset), then its members in
        // index order (1, 2, 3, 7, 8), then the xref stream object 5.
        assert_eq!(
            order,
            [
                (4, false),
                (1, true),
                (2, true),
                (3, true),
                (7, true),
                (8, true),
                (5, false),
            ]
        );
        // Member spans index into the decoded container and reparse cleanly.
        for element in &elements {
            let Element::IndirectObject { object, in_objstm: Some((container, member_span)), .. } = element else {
                continue;
            };
            assert_eq!(*container, ObjRef { num: 4, gen: 0 });
            let stm = doc.objstm_handle(4).unwrap();
            let (reparsed, range) = stm
                .object_spanned(
                    // Recover the member's index by matching its span.
                    (0..)
                        .map(|i| (i, stm.object_spanned(i)))
                        .take_while(|pair| pair.1.is_ok())
                        .find(|pair| {
                            pair.1.as_ref().unwrap().1
                                == (member_span.start as usize, member_span.end as usize)
                        })
                        .map(|pair| pair.0)
                        .expect("member span maps to an index"),
                )
                .unwrap();
            assert_eq!(reparsed, *object);
            assert_eq!(range, (member_span.start as usize, member_span.end as usize));
        }
    }

    #[test]
    fn xref_stream_docs_yield_stream_section_and_synthetic_trailer_span() {
        let data = pdfboss_testkit::objstm_doc(&[]);
        let doc = Document::load(data).unwrap();
        let elements = physical(&doc);
        let section = elements
            .iter()
            .find_map(|e| match e {
                Element::XrefSection { kind, span, .. } => Some((*kind, *span)),
                _ => None,
            })
            .expect("xref section present");
        assert_eq!(section.0, XrefKind::Stream);
        let trailer = elements
            .iter()
            .find_map(|e| match e {
                Element::Trailer { dict, span } => Some((dict.clone(), *span)),
                _ => None,
            })
            .expect("trailer present");
        assert!(trailer.0.get("Root").is_some());
        // No classic trailer keyword exists: the trailer span is the newest
        // xref stream object's span.
        assert_eq!(trailer.1, section.1);
    }

    #[test]
    fn broken_object_yields_err_and_iteration_continues() {
        let mut builder = pdfboss_testkit::PdfBuilder::new();
        builder.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        builder.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        builder.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
        builder.object(6, "<< /Broken >>");
        let mut data = builder.build(1);
        // Corrupt object 6's header in place: same length, no valid parse.
        let pos = memchr::memmem::find(&data, b"6 0 obj").unwrap();
        data[pos..pos + 7].copy_from_slice(b"6 ) obj");
        let doc = Document::load(data).unwrap();
        let opts = ElementOpts {
            logical: false,
            ..ElementOpts::default()
        };
        let items: Vec<Result<Element>> = doc.elements(opts).collect();
        assert!(items.iter().any(|i| i.is_err()), "corrupt object surfaces as Err");
        let good: Vec<u32> = items
            .iter()
            .filter_map(|i| match i {
                Ok(Element::IndirectObject { r, .. }) => Some(r.num),
                _ => None,
            })
            .collect();
        for num in [1u32, 2, 3] {
            assert!(good.contains(&num), "object {num} still iterates");
        }
        assert!(items.iter().any(|i| matches!(i, Ok(Element::Eof { .. }))));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core elements::tests -- --nocapture`
Expected: COMPILE ERROR — `Document::elements` and `Elements` not defined.

- [ ] **Step 3: Implement the physical state machine**

Add to `crates/pdfboss-core/src/elements.rs` (below the type definitions from Task 2, above `mod tests`). Extend the imports at the top of the file to:

```rust
use crate::content::Op;
use crate::document::Document;
use crate::error::{Error, Result};
use crate::hash::FastMap;
use crate::lexer::{Lexer, Token};
use crate::object::{Dict, Name, ObjRef, Object};
use crate::xref::{parse_section_at, XrefEntry};
```

then the implementation:

```rust
impl Document {
    /// Lazy iteration over the document's elements. Physical elements come
    /// in file order (header, objects by offset with object-stream members
    /// after their container, xref sections newest→oldest, trailer,
    /// startxref, eof); logical elements follow in document order. Nothing
    /// is parsed or decoded before it is yielded; an element that fails to
    /// parse yields `Err` for that item and iteration continues.
    pub fn elements(&self, opts: ElementOpts) -> Elements<'_> {
        Elements {
            doc: self,
            opts,
            stage: Stage::Start,
            container_spans: FastMap::default(),
        }
    }
}

/// Iterator state. Each `next()` parses at most one element.
pub struct Elements<'a> {
    doc: &'a Document,
    opts: ElementOpts,
    stage: Stage,
    /// File spans of already-parsed object-stream containers.
    container_spans: FastMap<u32, Span>,
}

enum Stage {
    Start,
    Objects { order: Vec<OrderEntry>, next: usize },
    Sections {
        next_offset: Option<usize>,
        visited: Vec<usize>,
        /// Newest classic trailer span, or newest stream-section span.
        trailer_span: Option<Span>,
    },
    Trailer { span: Option<Span> },
    StartXref,
    Eof,
    Logical { page: usize, part: PagePart },
    Done,
}

/// Sub-state within one page during logical iteration (Task 8 fills the
/// variants in; the physical layer only needs the entry point).
enum PagePart {
    PageItself,
}

/// One object scheduled for physical iteration, pre-sorted by file position.
struct OrderEntry {
    num: u32,
    entry: XrefEntry,
    /// The object's own offset, or its container's offset for members.
    sort_offset: u64,
    /// 0 for in-file objects; 1 + member index for object-stream members,
    /// so members directly follow their container.
    sort_member: u64,
}

impl<'a> Iterator for Elements<'a> {
    type Item = Result<Element>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match &mut self.stage {
                Stage::Start => {
                    let order = if self.opts.physical {
                        build_order(self.doc)
                    } else {
                        Vec::new()
                    };
                    let header = self.opts.physical.then(|| header_element(self.doc));
                    self.stage = Stage::Objects { order, next: 0 };
                    if let Some(Some(header)) = header {
                        return Some(Ok(header));
                    }
                }
                Stage::Objects { order, next } => {
                    if *next >= order.len() {
                        self.stage = if self.opts.physical {
                            Stage::Sections {
                                next_offset: find_startxref_offset(self.doc.bytes()),
                                visited: Vec::new(),
                                trailer_span: None,
                            }
                        } else {
                            Stage::Logical { page: 0, part: PagePart::PageItself }
                        };
                        continue;
                    }
                    let index = *next;
                    *next += 1;
                    let entry = &order[index];
                    let item = self.object_element(entry.num, entry.entry);
                    return Some(item);
                }
                Stage::Sections { next_offset, visited, trailer_span } => {
                    let Some(off) = *next_offset else {
                        self.stage = Stage::Trailer { span: *trailer_span };
                        continue;
                    };
                    if visited.contains(&off) {
                        self.stage = Stage::Trailer { span: *trailer_span };
                        continue;
                    }
                    visited.push(off);
                    match parse_section_at(self.doc.bytes(), off) {
                        Ok(info) => {
                            if trailer_span.is_none() {
                                *trailer_span = info.trailer_span.or(Some(info.span));
                            }
                            *next_offset = info
                                .prev
                                .and_then(|v| usize::try_from(v).ok())
                                .filter(|&o| o < self.doc.bytes().len());
                            let element = Element::XrefSection {
                                kind: info.kind,
                                span: info.span,
                                entries: info.xref.len(),
                            };
                            return Some(Ok(element));
                        }
                        Err(err) => {
                            // Salvage: report the broken section, then stop
                            // walking the chain.
                            *next_offset = None;
                            return Some(Err(err));
                        }
                    }
                }
                Stage::Trailer { span } => {
                    let span = *span;
                    self.stage = Stage::StartXref;
                    if let Some(span) = span {
                        return Some(Ok(Element::Trailer {
                            dict: self.doc.xref().trailer.clone(),
                            span,
                        }));
                    }
                }
                Stage::StartXref => {
                    self.stage = Stage::Eof;
                    if let Some(element) = startxref_element(self.doc.bytes()) {
                        return Some(Ok(element));
                    }
                }
                Stage::Eof => {
                    self.stage = Stage::Logical { page: 0, part: PagePart::PageItself };
                    if let Some(element) = eof_element(self.doc.bytes()) {
                        return Some(Ok(element));
                    }
                }
                Stage::Logical { .. } => {
                    // Task 8 implements the logical layer; until then it ends
                    // the iteration.
                    self.stage = Stage::Done;
                }
                Stage::Done => return None,
            }
        }
    }
}

impl<'a> Elements<'a> {
    /// Builds the `IndirectObject` element for one xref entry.
    fn object_element(&mut self, num: u32, entry: XrefEntry) -> Result<Element> {
        match entry {
            XrefEntry::Free => Err(Error::ObjectNotFound(num, 0)),
            XrefEntry::InFile { offset, .. } => {
                let offset = usize::try_from(offset)
                    .ok()
                    .filter(|&o| o < self.doc.bytes().len())
                    .ok_or(Error::ObjectNotFound(num, 0))?;
                let (r, object, span) = self.doc.object_at_spanned(offset)?;
                self.container_spans.insert(r.num, span);
                Ok(Element::IndirectObject { r, object, span, in_objstm: None })
            }
            XrefEntry::InStream { stream_num, index } => {
                let container_span = self.container_span(stream_num)?;
                let stm = self.doc.objstm_handle(stream_num)?;
                let (object, (start, end)) = stm.object_spanned(index)?;
                Ok(Element::IndirectObject {
                    r: ObjRef { num, gen: 0 },
                    object,
                    span: container_span,
                    in_objstm: Some((
                        ObjRef { num: stream_num, gen: 0 },
                        Span::new(start as u64, end as u64),
                    )),
                })
            }
        }
    }

    /// The file span of an object-stream container, parsed at most once.
    fn container_span(&mut self, stream_num: u32) -> Result<Span> {
        if let Some(span) = self.container_spans.get(&stream_num) {
            return Ok(*span);
        }
        let offset = match self.doc.xref().get(stream_num) {
            Some(XrefEntry::InFile { offset, .. }) => usize::try_from(offset)
                .ok()
                .filter(|&o| o < self.doc.bytes().len())
                .ok_or(Error::ObjectNotFound(stream_num, 0))?,
            _ => return Err(Error::ObjectNotFound(stream_num, 0)),
        };
        let (.., span) = self.doc.object_at_spanned(offset)?;
        self.container_spans.insert(stream_num, span);
        Ok(span)
    }
}

/// All live objects sorted into file order: in-file objects by offset, then
/// object-stream members grouped after their container by member index.
fn build_order(doc: &Document) -> Vec<OrderEntry> {
    let mut order: Vec<OrderEntry> = doc
        .xref()
        .iter()
        .filter_map(|(num, entry)| match entry {
            XrefEntry::Free => None,
            XrefEntry::InFile { offset, .. } => Some(OrderEntry {
                num,
                entry,
                sort_offset: offset,
                sort_member: 0,
            }),
            XrefEntry::InStream { stream_num, index } => {
                let container_offset = match doc.xref().get(stream_num) {
                    Some(XrefEntry::InFile { offset, .. }) => offset,
                    // A member whose container is missing sorts last and
                    // surfaces as Err from object_element.
                    None | Some(XrefEntry::Free) | Some(XrefEntry::InStream { .. }) => u64::MAX,
                };
                Some(OrderEntry {
                    num,
                    entry,
                    sort_offset: container_offset,
                    sort_member: 1 + u64::from(index),
                })
            }
        })
        .collect();
    order.sort_by_key(|e| (e.sort_offset, e.sort_member, e.num));
    order
}
```

and the free helpers:

```rust
/// The `%PDF-x.y` header element, when a header is physically present.
fn header_element(doc: &Document) -> Option<Element> {
    let data = doc.bytes();
    let window = &data[..data.len().min(1024)];
    let pos = memchr::memmem::find(window, b"%PDF-")?;
    let digits_end = window[pos + 5..]
        .iter()
        .position(|&b| !(b.is_ascii_digit() || b == b'.'))
        .map(|rel| pos + 5 + rel)
        .unwrap_or(window.len());
    Some(Element::Header {
        version: doc.version(),
        span: Span::new(pos as u64, digits_end as u64),
    })
}

/// The byte offset announced by the last `startxref` keyword (the offset the
/// section walk starts from), bounded to the file.
fn find_startxref_offset(data: &[u8]) -> Option<usize> {
    let tail = data.len().saturating_sub(64 * 1024);
    let rel = memchr::memmem::rfind(&data[tail..], b"startxref")?;
    let mut lexer = Lexer::at(data, tail + rel + b"startxref".len());
    match lexer.next_token() {
        Ok(Token::Int(v)) => usize::try_from(v).ok().filter(|&o| o < data.len()),
        _ => None,
    }
}

/// The `startxref` element: keyword through its integer operand.
fn startxref_element(data: &[u8]) -> Option<Element> {
    let tail = data.len().saturating_sub(64 * 1024);
    let rel = memchr::memmem::rfind(&data[tail..], b"startxref")?;
    let start = tail + rel;
    let mut lexer = Lexer::at(data, start + b"startxref".len());
    match lexer.next_token() {
        Ok(Token::Int(v)) if v >= 0 => Some(Element::StartXref {
            offset: v as u64,
            span: Span::new(start as u64, lexer.pos() as u64),
        }),
        _ => None,
    }
}

/// The last `%%EOF` marker.
fn eof_element(data: &[u8]) -> Option<Element> {
    let tail = data.len().saturating_sub(64 * 1024);
    let rel = memchr::memmem::rfind(&data[tail..], b"%%EOF")?;
    let start = tail + rel;
    Some(Element::Eof {
        span: Span::new(start as u64, (start + b"%%EOF".len()) as u64),
    })
}
```

Note on `Stage::Start`: `header_element` returns `Option<Element>`; the stage advances first and yields the header only when present, so headerless (recovered) files iterate straight into objects. Note on `container_span`: it parses the container at most once per iterator; the `(.., span)` tuple pattern takes the last field without naming discards.

- [ ] **Step 4: Run the whole core suite**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core`
Expected: all tests PASS. Clippy clean (`-D warnings`) — in particular no `dead_code` on `PagePart` (it is constructed in `Stage::Logical` transitions).

- [ ] **Step 5: Commit**

```bash
git add crates/pdfboss-core/src/elements.rs
git commit -m "feat(core): lazy physical element iteration with byte spans"
```

---

### Task 8: `Elements` iterator — logical layer (pages, fonts, images, annotations)

**Files:**
- Modify: `crates/pdfboss-core/src/elements.rs`
- Test: inline `mod tests`

**Interfaces:**
- Consumes: Task 7 state machine, `Document::{page, page_count, resolve, get}`, `Page::{resources, dict, object_ref}`.
- Produces: `Element::Page` / `Font` / `Image` / `Annotation` items in document order — per page: the page, then fonts (resource-name order), images (resource-name order), annotations (array order). `opts.pages` restricts which pages contribute; `opts.logical = false` skips the layer entirely.

- [ ] **Step 1: Write the failing tests**

Append to `elements.rs` tests:

```rust
    #[test]
    fn logical_walk_reports_page_fonts_images_annots() {
        let mut builder = pdfboss_testkit::PdfBuilder::new();
        builder.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        builder.object(
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        );
        builder.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 7 0 R >> >> \
             /Annots [8 0 R] >>",
        );
        builder.object(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        builder.stream(
            7,
            "/Type /XObject /Subtype /Image /Width 2 /Height 3 \
             /ColorSpace /DeviceGray /BitsPerComponent 8",
            &[0, 1, 2, 3, 4, 5],
        );
        builder.object(8, "<< /Type /Annot /Subtype /Link >>");
        let doc = Document::load(builder.build(1)).unwrap();

        let opts = ElementOpts {
            physical: false,
            ..ElementOpts::default()
        };
        let elements: Vec<Element> =
            doc.elements(opts).collect::<Result<Vec<_>>>().unwrap();

        let kinds: Vec<&str> = elements
            .iter()
            .map(|e| match e {
                Element::Page { .. } => "page",
                Element::Font { .. } => "font",
                Element::Image { .. } => "image",
                Element::Annotation { .. } => "annot",
                other => panic!("unexpected element in logical-only walk: {other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["page", "font", "image", "annot"]);

        let Element::Page { index, r } = &elements[0] else { unreachable!() };
        assert_eq!(*index, 0);
        assert_eq!(*r, ObjRef { num: 3, gen: 0 });
        let Element::Font { page, r, subtype, base_font } = &elements[1] else {
            unreachable!()
        };
        assert_eq!(*page, Some(0));
        assert_eq!(*r, ObjRef { num: 6, gen: 0 });
        assert_eq!(subtype.0, "Type1");
        assert_eq!(base_font.as_ref().map(|n| n.0.as_str()), Some("Helvetica"));
        let Element::Image { page, r, width, height } = &elements[2] else {
            unreachable!()
        };
        assert_eq!(*page, Some(0));
        assert_eq!(*r, ObjRef { num: 7, gen: 0 });
        assert_eq!((*width, *height), (2, 3));
        let Element::Annotation { page, r, subtype } = &elements[3] else {
            unreachable!()
        };
        assert_eq!(*page, 0);
        assert_eq!(*r, ObjRef { num: 8, gen: 0 });
        assert_eq!(subtype.0, "Link");
    }

    #[test]
    fn pages_filter_restricts_logical_elements() {
        let doc =
            Document::load(pdfboss_testkit::multi_page_doc(&["a", "b", "c"])).unwrap();
        let opts = ElementOpts {
            physical: false,
            pages: Some(vec![1]),
            ..ElementOpts::default()
        };
        let pages: Vec<usize> = doc
            .elements(opts)
            .filter_map(|item| match item {
                Ok(Element::Page { index, .. }) => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(pages, [1]);
    }

    #[test]
    fn full_walk_yields_physical_then_logical() {
        let doc = Document::load(pdfboss_testkit::simple_doc("both")).unwrap();
        let elements: Vec<Element> = doc
            .elements(ElementOpts::default())
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let eof_pos = elements
            .iter()
            .position(|e| matches!(e, Element::Eof { .. }))
            .expect("eof present");
        let first_page = elements
            .iter()
            .position(|e| matches!(e, Element::Page { .. }))
            .expect("page present");
        assert!(first_page > eof_pos, "logical elements follow physical ones");
        // simple_doc has a /Font resource: it must surface.
        assert!(elements.iter().any(|e| matches!(e, Element::Font { .. })));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core logical_walk -- --nocapture`
Expected: FAIL — the logical-only walk yields nothing (Task 7 stubs `Stage::Logical` into `Done`), so `elements` is empty and the first assertion panics.

- [ ] **Step 3: Implement the logical layer**

In `elements.rs`, replace the `PagePart` stub and the `Stage::Logical` arm.

New `PagePart` (a queue of already-materialized elements plus what remains to compute — computation happens page-by-page, so memory stays bounded by one page's element count):

```rust
/// Logical iteration works page-by-page: entering a page materializes that
/// page's elements into a queue (bounded by one page), which then drains
/// one `next()` at a time.
enum PagePart {
    PageItself,
    Drain { queue: std::collections::VecDeque<Result<Element>> },
}
```

Replace the `Stage::Logical` arm of `Iterator::next`:

```rust
                Stage::Logical { page, part } => {
                    if !self.opts.logical || *page >= self.doc.page_count() {
                        self.stage = Stage::Done;
                        continue;
                    }
                    let index = *page;
                    let selected = self
                        .opts
                        .pages
                        .as_ref()
                        .map(|list| list.contains(&index))
                        .unwrap_or(true);
                    match part {
                        PagePart::PageItself => {
                            if !selected {
                                *page += 1;
                                continue;
                            }
                            let queue = self.page_elements(index);
                            *part = PagePart::Drain { queue };
                        }
                        PagePart::Drain { queue } => match queue.pop_front() {
                            Some(item) => return Some(item),
                            None => {
                                *page += 1;
                                *part = PagePart::PageItself;
                            }
                        },
                    }
                }
```

Add the page materializer to `impl<'a> Elements<'a>`:

```rust
    /// Materializes one page's logical elements, in document order: the page
    /// itself, fonts, images, annotations (content ops are appended by the
    /// content-op stage when enabled). Broken pieces surface as `Err` items.
    fn page_elements(&self, index: usize) -> std::collections::VecDeque<Result<Element>> {
        let mut queue = std::collections::VecDeque::new();
        let page = match self.doc.page(index) {
            Ok(page) => page,
            Err(err) => {
                queue.push_back(Err(err));
                return queue;
            }
        };
        if let Some(r) = page.object_ref() {
            queue.push_back(Ok(Element::Page { index, r }));
        }
        // Fonts: /Resources /Font — a dict of name → (usually) reference.
        for (r, dict) in self.referenced_dict_entries(page.resources.get("Font")) {
            let subtype = dict
                .get_name("Subtype")
                .cloned()
                .unwrap_or_else(|| Name(String::new()));
            let base_font = dict.get_name("BaseFont").cloned();
            queue.push_back(Ok(Element::Font {
                page: Some(index),
                r,
                subtype,
                base_font,
            }));
        }
        // Images: /Resources /XObject entries whose /Subtype is /Image.
        for (r, dict) in self.referenced_dict_entries(page.resources.get("XObject")) {
            if dict.get_name("Subtype").map(|n| n.0.as_str()) != Some("Image") {
                continue;
            }
            let width = dict.get_int("Width").and_then(|v| u32::try_from(v).ok());
            let height = dict.get_int("Height").and_then(|v| u32::try_from(v).ok());
            queue.push_back(Ok(Element::Image {
                page: Some(index),
                r,
                width: width.unwrap_or(0),
                height: height.unwrap_or(0),
            }));
        }
        // Annotations: the page dict's /Annots array of references.
        if let Some(annots) = page.dict().get("Annots") {
            if let Ok(Object::Array(items)) = self.doc.resolve(annots) {
                for item in items {
                    let Object::Ref(r) = item else { continue };
                    let Ok(resolved) = self.doc.resolve(&Object::Ref(r)) else {
                        continue;
                    };
                    let Some(dict) = resolved.as_dict() else { continue };
                    let subtype = dict
                        .get_name("Subtype")
                        .cloned()
                        .unwrap_or_else(|| Name(String::new()));
                    queue.push_back(Ok(Element::Annotation { page: index, r, subtype }));
                }
            }
        }
        queue
    }

    /// Resolves a resource-category value (e.g. the `/Font` entry) to its
    /// dictionary and yields, in name order, each entry that is a reference
    /// to a dictionary or stream — as `(reference, dictionary)`. Entries
    /// inlined without a reference are skipped (they have no identity to
    /// report); name order keeps iteration deterministic.
    fn referenced_dict_entries(&self, category: Option<&Object>) -> Vec<(ObjRef, Dict)> {
        let Some(category) = category else {
            return Vec::new();
        };
        let Ok(resolved) = self.doc.resolve(category) else {
            return Vec::new();
        };
        let Some(dict) = resolved.as_dict() else {
            return Vec::new();
        };
        let mut names: Vec<&Name> = dict.iter().map(|entry| entry.0).collect();
        names.sort();
        let mut out = Vec::new();
        for name in names {
            let Some(Object::Ref(r)) = dict.get(&name.0) else {
                continue;
            };
            let r = *r;
            let Ok(target) = self.doc.resolve(&Object::Ref(r)) else {
                continue;
            };
            let target_dict = match &target {
                Object::Dict(d) => d.clone(),
                Object::Stream(s) => s.dict.clone(),
                _ => continue,
            };
            out.push((r, target_dict));
        }
        out
    }
```

(`Dict::get` takes `&str` — confirm against `crates/pdfboss-core/src/object.rs:28` `Dict` impl while implementing; if `get` takes `&Name` adjust the call to `dict.get(name)` accordingly. `dict.iter()` yields `(&Name, &Object)` — as used by `pretty.rs` and `Xref::merge`.)

- [ ] **Step 4: Run the whole core suite**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core`
Expected: all tests PASS. Clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pdfboss-core/src/elements.rs
git commit -m "feat(core): logical element iteration (pages, fonts, images, annotations)"
```

---

### Task 9: `Elements` iterator — content operators

**Files:**
- Modify: `crates/pdfboss-core/src/elements.rs`
- Test: inline `mod tests`

**Interfaces:**
- Consumes: Task 5 `parse_content_spanned`, Task 8 page queue, `Page::content(&self, doc)`.
- Produces: `Element::ContentOp { page, op, span_in_content }` items appended after a page's annotations when `opts.content_ops` is true. `span_in_content` indexes the page's decoded, concatenated content exactly as `Page::content` returns it.

- [ ] **Step 1: Write the failing test**

Append to `elements.rs` tests:

```rust
    #[test]
    fn content_ops_are_spanned_against_page_content() {
        let doc = Document::load(pdfboss_testkit::simple_doc("ops!")).unwrap();
        let opts = ElementOpts {
            physical: false,
            content_ops: true,
            ..ElementOpts::default()
        };
        let ops: Vec<(usize, Op, Span)> = doc
            .elements(opts)
            .filter_map(|item| match item {
                Ok(Element::ContentOp { page, op, span_in_content }) => {
                    Some((page, op, span_in_content))
                }
                _ => None,
            })
            .collect();
        assert!(!ops.is_empty(), "simple_doc paints text: ops must appear");
        let content = doc.page(0).unwrap().content(&doc).unwrap();
        for (page, op, span) in &ops {
            assert_eq!(*page, 0);
            let slice = &content[span.start as usize..span.end as usize];
            let reparsed = crate::content::parse_content(slice).unwrap();
            assert_eq!(reparsed.len(), 1);
            assert_eq!(&reparsed[0], op);
        }
    }

    #[test]
    fn content_ops_default_off() {
        let doc = Document::load(pdfboss_testkit::simple_doc("quiet")).unwrap();
        let none = doc
            .elements(ElementOpts::default())
            .all(|item| !matches!(item, Ok(Element::ContentOp { .. })));
        assert!(none);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core content_ops_are_spanned -- --nocapture`
Expected: FAIL — `ops` is empty (no ContentOp is ever yielded).

- [ ] **Step 3: Implement**

In `Elements::page_elements` (Task 8), append before `queue` is returned:

```rust
        // Content operators, when requested: parsed against the page's
        // decoded, concatenated content.
        if self.opts.content_ops {
            match page.content(self.doc) {
                Ok(content) => match crate::content::parse_content_spanned(&content) {
                    Ok(spanned) => {
                        for (op, span) in spanned {
                            queue.push_back(Ok(Element::ContentOp {
                                page: index,
                                op,
                                span_in_content: span,
                            }));
                        }
                    }
                    Err(err) => queue.push_back(Err(err)),
                },
                Err(err) => queue.push_back(Err(err)),
            }
        }
```

- [ ] **Step 4: Run the whole core suite, clippy, fmt**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-core && CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-core --all-targets -- -D warnings && cargo fmt --all --check`
Expected: everything PASSES/clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pdfboss-core/src/elements.rs
git commit -m "feat(core): content-operator elements with in-content spans"
```

---

### Task 10: Workspace-wide verification and doc touch-up

**Files:**
- Modify: `crates/pdfboss-core/src/lib.rs` (crate docs), `README.md` (feature list bullet)

**Interfaces:** none new.

- [ ] **Step 1: Extend the crate doc comment**

In `crates/pdfboss-core/src/lib.rs`, extend the crate-level doc comment's first paragraph to mention element iteration:

```rust
//! Core PDF machinery: syntax, objects, filters, cross-references, the
//! document model, and lazy element iteration (physical file structure with
//! byte spans plus logical document structure), implemented from the PDF
//! specification (ISO 32000).
```

- [ ] **Step 2: README bullet**

In `README.md`, add one bullet to the existing feature list (match the list's style exactly as found):

```markdown
- Element iteration: walk every physical element (objects, xref sections, trailer — with byte spans) and logical element (pages, fonts, images, annotations, content operators) of a document lazily.
```

- [ ] **Step 3: Full workspace verification**

Run: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test --workspace && CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check && CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo doc -p pdfboss-core --no-deps`
Expected: all green — every pre-existing test in text/render/cli/py still passes untouched.

- [ ] **Step 4: Commit**

```bash
git add crates/pdfboss-core/src/lib.rs README.md
git commit -m "docs(core): document element iteration"
```

---

## Self-review notes (already applied)

- Spec coverage: Span/Element/ElementOpts/XrefKind (Task 2), `Document::elements` lazy + salvage + ordering (Tasks 7–9), parser span plumbing (none needed — `Parser::pos()` suffices; content and objstm needed real additions, Tasks 5–6), `pretty` move (Task 1). Public extras the spec implies but does not name (`Document::bytes/xref`, `Xref::iter`, `parse_section_at`, `Page::object_ref`, testkit `objstm_doc`) are pinned here in Tasks 3–4/6 — plans 02/04/05 consume these exact signatures.
- Type consistency: `Span{start,end}: u64` everywhere; `ObjStm::object_spanned` returns `(Object, (usize, usize))` (decoded-buffer coordinates, converted to `Span` only at the element boundary).
- Known judgment calls: pages without an indirect ref yield no `Page` element (children still appear); fonts/images/annots inlined without references are skipped; a broken xref chain stops section iteration after one `Err`; `Trailer.span` falls back to the newest stream section's span when no classic trailer exists.
