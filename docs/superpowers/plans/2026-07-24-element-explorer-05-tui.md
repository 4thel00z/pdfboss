# pdfboss TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the `pdfboss-tui` library crate (ratatui explorer: lazy tree, pretty inspector, hexyl-style hex pane, half-block page preview, incremental search) plus the `pdfboss tui <file-or-url>` CLI subcommand, per the 2026-07-24 element-explorer spec.

**Architecture:** All state lives in a pure `App` struct: `App::update(Msg) -> Vec<Cmd>` mutates state and returns side-effect descriptions; `ui::draw(&App, &mut Frame)` renders; a thin `tokio::select!` event loop in `lib.rs` turns crossterm events and background-task completions into `Msg`s and executes `Cmd`s by spawning tasks against a cloned `AsyncDocument`. Data arrives lazily: tree sections populate from `AsyncDocument::elements` on first expand, hex bytes via `read_span` windows, previews via `pdfboss-render` inside `spawn_blocking`.

**Tech Stack:** Rust (edition 2021), ratatui 0.29, crossterm 0.28 (`event-stream`), tokio (current-thread rt), futures-util, pdfboss-aio (plan 02), pdfboss-core `elements`/`pretty` (plan 01), pdfboss-render, pdfboss-testkit (dev).

**Prerequisites:** Plans 01 and 02 of this series are merged: `pdfboss_core::elements::{Span, Element, ElementOpts, XrefKind}` and `pdfboss_core::pretty::{format_object, format_dict}` exist, and `pdfboss_aio::AsyncDocument` provides `open / open_url (http feature) / from_bytes / get_object / resolve / decode_stream / read_span / metadata / page_count / version / elements -> ElementStream`, `Send + Sync + Clone`.

## Global Constraints

- **Cleanroom rule (from the spec, applies unchanged):** everything is implemented from ISO 32000; **never name any other PDF library anywhere** — code, comments, docs, tests, commit messages, plan prose. Non-PDF dependencies (tokio, ratatui, crossterm, futures) are fine to name.
- **`pdfboss-core` gains zero new dependencies.** No async, no serde anywhere in core. This plan touches core not at all.
- **The existing sync API (`Document`, `Page`, text, render) and all existing subcommands and tests stay untouched.** New capability is additive.
- **Never create underscore-prefixed identifiers** (no `_foo` methods/fields/locals/bindings — use full names; in match arms prefer named bindings or `..`; the bare wildcard pattern `_` is not an identifier and is fine).
- Edition 2021; `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` stay clean after every task.
- **Shared build cache:** every cargo invocation uses `CARGO_TARGET_DIR=$HOME/.cargo/shared-target`; never per-agent target dirs.

## Resolved ambiguities (decided here, consumed by every task)

1. **Release-please registration:** this repo versions all workspace crates through the root package — crates set `version.workspace = true`, and the single `# x-release-please-version` marker lives in the root `Cargo.toml` `[workspace.package]`, which the root release-please package rewrites via `extra-files: ["Cargo.toml"]`. Adding `crates/pdfboss-tui` to `release-please-config.json`/`.release-please-manifest.json` as its own package would fork a second version stream diverging from the workspace (no existing Rust crate is registered individually; only `packages/pdfboss-fonts` is, because it releases independently). `pdfboss-tui` therefore registers the same way `pdfboss-render` does: workspace member + `version.workspace = true`, riding the root marker. Task 1 verifies this wiring; the config/manifest JSON files are not modified.
2. **`run` error type:** the spec pins `pub async fn run(doc: AsyncDocument, title: String) -> Result<()>` without naming the error. Terminal setup/draw failures are the only errors `run` can not handle internally (document errors become status-bar toasts), so `Result` is `std::io::Result<()>`.
3. **Preview needs the sync renderer:** `pdfboss_render::render_page(doc: &Document, page: &Page, scale: f32) -> Result<Pixmap>` takes the sync `pdfboss_core::Document`, which is not `Send` (interior `Rc`/`RefCell`). On the first preview request the executor fetches the full file once via `doc.read_span(Span { start: 0, end: doc.file_len() }).await` — plan 02 exposes the sync length accessor `AsyncDocument::file_len(&self) -> u64`, available immediately after open — caches the bytes as `Arc<Vec<u8>>` in `PreviewState`, and every render constructs a fresh `Document::load` **inside** `spawn_blocking` (created and dropped entirely within the closure). Whole-file fetch happens only when the user asks for a preview, and only the preview needs the file length (the hex pane always reads the selection's own span).
4. **Hex line width:** 8 bytes per line (`offset │ hex │ ascii` ≈ 45 columns) so lines fit the right pane at 80 columns; hexyl-style byte-class colors are kept. Windowed fetching: 64 KiB windows so huge spans never load wholesale.
5. **Tree top level** follows the spec sketch exactly: Document → Pages → Objects → Xref → Trailer. `StartXref` and `%%EOF` elements appear as leaves inside the Xref folder; the file header has no node — selecting the Document root shows version/page-count in the inspector (hex pane stays empty for folder-ish selections).
6. **Laziness is strict:** nothing streams at startup. First expand of Pages triggers the logical pass; first expand of Objects *or* Xref (or a jump/Trailer selection needing physical data) triggers the physical pass, which populates Objects, Xref sections, trailer dict and header in one stream (streaming physical elements parses every object anyway, so splitting passes would double the work). A page's Contents folder fetches that page's `/Contents` refs on first expand.
7. **Inspector `d` cycle:** Pretty → Raw → Decoded → Ops → Pretty, available for stream objects only (`d` on a non-stream toasts). Ops disassembles via `pdfboss_core::content::parse_content` on the decoded bytes; a parse failure renders as an error line inside the view (any stream may be disassembled; content streams are the intended case per the spec).
8. **Search domain:** matches object numbers, dict keys, name values and string contents of physical `IndirectObject` elements (the spec's "visits objects lazily"); results stream over an mpsc channel tagged with a generation; stale generations are dropped and the search task also self-terminates by polling a shared `AtomicU64` epoch.

---

### Task 1: Crate scaffold and workspace registration

**Files:**
- Create: `crates/pdfboss-tui/Cargo.toml`
- Create: `crates/pdfboss-tui/src/lib.rs`
- Modify: `Cargo.toml` (root; members list, lines 3–11)
- Test: `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo check -p pdfboss-tui`

**Interfaces:**
- Consumes: `pdfboss_aio` crate (plan 02), `pdfboss_core` (plan 01), `pdfboss_render` — as path deps only in this task.
- Produces: an empty publishable `pdfboss-tui` workspace member later tasks fill in.

**Steps:**

- [ ] Create `crates/pdfboss-tui/Cargo.toml` (workspace-versioned like `pdfboss-render`; the `# x-release-please-version` marker in the root `Cargo.toml` governs the version — see Resolved ambiguity 1):

```toml
[package]
name = "pdfboss-tui"
description = "Terminal explorer for PDF internals: element tree, object inspector, hex view and page preview"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
pdfboss-aio = { path = "../pdfboss-aio" }
pdfboss-core = { path = "../pdfboss-core" }
pdfboss-render = { path = "../pdfboss-render" }
ratatui = "0.29"
crossterm = { version = "0.28", features = ["event-stream"] }
tokio = { version = "1", features = ["rt", "macros", "sync", "time"] }
futures-util = "0.3"

[dev-dependencies]
pdfboss-testkit = { path = "../pdfboss-testkit" }
```

- [ ] Create `crates/pdfboss-tui/src/lib.rs`:

```rust
//! Interactive terminal explorer for PDF internals, implemented from
//! ISO 32000 on top of `pdfboss-aio`'s async document model.
//!
//! State machine (`app`), pane models (`tree`, `inspector`, `hexview`,
//! `preview`, `search`), key mapping (`input`) and rendering (`ui`) are
//! pure and unit-testable; only [`run`] touches the real terminal.
```

- [ ] Modify root `Cargo.toml` members (keep existing entries; plan 02 already added `pdfboss-aio`):

```toml
members = [
    "crates/pdfboss-core",
    "crates/pdfboss-text",
    "crates/pdfboss-encoding",
    "crates/pdfboss-render",
    "crates/pdfboss-aio",
    "crates/pdfboss-cli",
    "crates/pdfboss-py",
    "crates/pdfboss-testkit",
    "crates/pdfboss-tui",
]
```

- [ ] Verify release/CI wiring needs no edits: `grep -n "x-release-please-version" Cargo.toml` shows the workspace marker (line 14); `.github/workflows/ci.yaml` runs `--workspace` for fmt/clippy/test/doc, so the new member rides the existing matrix automatically. Confirm `release-please-config.json` still lists only `.` and `packages/pdfboss-fonts`.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo check -p pdfboss-tui` — expect success (empty lib compiles; deps resolve).
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check` — expect clean.
- [ ] Commit: `git add Cargo.toml Cargo.lock crates/pdfboss-tui && git commit -m "feat(tui): scaffold pdfboss-tui workspace crate"`

---

### Task 2: Tree pane state machine (`tree.rs`)

**Files:**
- Create: `crates/pdfboss-tui/src/tree.rs`
- Modify: `crates/pdfboss-tui/src/lib.rs` (add module declaration)
- Test: unit tests inside `crates/pdfboss-tui/src/tree.rs`

**Interfaces:**
- Consumes (plan 01, spec-pinned): `pdfboss_core::elements::{Element, Span, XrefKind}` with `Element::{Header{version,span}, IndirectObject{r,object,span,in_objstm}, XrefSection{kind,span,entries}, Trailer{dict,span}, StartXref{offset,span}, Eof{span}, Page{index,r}, Font{page,r,subtype,base_font}, Image{page,r,width,height}, Annotation{page,r,subtype}, ContentOp{page,op,span_in_content}}`; `pdfboss_core::{Dict, Name, ObjRef}`.
- Contract note (cross-review, plan 01/02 parity): xref sections stream in chain order (newest → oldest) and every document yields exactly **one** `Trailer` element (the merged dict). The tree displays sections in arrival order and stores the trailer by plain assignment.
- Produces: `TreeState::new(version, page_count)`, `apply_batch(TreeReq, &[Element], done)`, `apply_contents(page, &[ObjRef])`, `mark_failed(TreeReq)`, `expand(NodeId) -> Option<TreeReq>`, `collapse_or_parent(NodeId) -> bool`, `visible_rows() -> Vec<TreeRow>`, `label(NodeId) -> String`, `breadcrumb() -> String`, `find_object(ObjRef) -> Option<NodeId>`, `reveal(NodeId)`, `selection_ref(NodeId) -> Option<ObjRef>`, `object_span(u32) -> Option<(Span, Option<(ObjRef, Span)>)>`, `page_ref(usize) -> Option<ObjRef>`, `page_of(NodeId) -> Option<usize>`, `select_next/select_prev/select_top/select_bottom`, plus `NodeId`, `Node`, `NodeKind`, `LoadState`, `TreeReq`, `TreeRow`.

**Steps:**

- [ ] Write the failing tests. Create `crates/pdfboss-tui/src/tree.rs` containing ONLY this test module for now (the `use super::*` items do not exist yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::elements::{Element, Span, XrefKind};
    use pdfboss_core::{Dict, Name, Object, ObjRef};

    fn obj_ref(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    fn physical_batch() -> Vec<Element> {
        vec![
            Element::Header {
                version: (1, 7),
                span: Span { start: 0, end: 15 },
            },
            Element::IndirectObject {
                r: obj_ref(1),
                object: Object::Null,
                span: Span { start: 15, end: 64 },
                in_objstm: None,
            },
            Element::IndirectObject {
                r: obj_ref(2),
                object: Object::Null,
                span: Span { start: 64, end: 120 },
                in_objstm: Some((obj_ref(9), Span { start: 4, end: 30 })),
            },
            Element::XrefSection {
                kind: XrefKind::Table,
                span: Span { start: 120, end: 260 },
                entries: 3,
            },
            Element::Trailer {
                dict: Dict::new(),
                span: Span { start: 260, end: 300 },
            },
            Element::StartXref {
                offset: 120,
                span: Span { start: 300, end: 314 },
            },
            Element::Eof {
                span: Span { start: 314, end: 320 },
            },
        ]
    }

    fn logical_batch() -> Vec<Element> {
        vec![
            Element::Page {
                index: 0,
                r: obj_ref(3),
            },
            Element::Font {
                page: Some(0),
                r: obj_ref(5),
                subtype: Name("Type1".to_string()),
                base_font: Some(Name("Helvetica".to_string())),
            },
            Element::Image {
                page: Some(0),
                r: obj_ref(7),
                width: 32,
                height: 16,
            },
            Element::Annotation {
                page: 0,
                r: obj_ref(8),
                subtype: Name("Link".to_string()),
            },
        ]
    }

    #[test]
    fn new_tree_has_root_and_four_sections() {
        let tree = TreeState::new((1, 7), 14);
        let rows = tree.visible_rows();
        let labels: Vec<String> = rows.iter().map(|row| tree.label(row.id)).collect();
        assert_eq!(
            labels,
            vec![
                "Document · PDF 1.7",
                "Pages (14)",
                "Objects",
                "Xref",
                "Trailer",
            ]
        );
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1);
        assert!(tree.node(tree.root).expanded);
        assert!(!tree.node(tree.pages_folder).expanded);
    }

    #[test]
    fn physical_batch_populates_objects_xref_and_trailer() {
        let mut tree = TreeState::new((1, 7), 1);
        assert_eq!(tree.expand(tree.objects_folder), Some(TreeReq::Physical));
        assert_eq!(tree.physical, LoadState::Loading);
        tree.apply_batch(TreeReq::Physical, &physical_batch(), true);
        assert_eq!(tree.physical, LoadState::Loaded);
        assert_eq!(tree.label(tree.objects_folder), "Objects (2)");
        assert_eq!(tree.label(tree.xref_folder), "Xref (1 secs)");
        assert_eq!(tree.header_span, Some(Span { start: 0, end: 15 }));
        assert_eq!(tree.trailer_span, Some(Span { start: 260, end: 300 }));
        assert!(tree.trailer_dict.is_some());
        let object_ids = tree.node(tree.objects_folder).children.clone();
        assert_eq!(tree.label(object_ids[0]), "obj 1 0");
        assert_eq!(tree.label(object_ids[1]), "obj 2 0");
        assert_eq!(
            tree.object_span(1),
            Some((Span { start: 15, end: 64 }, None))
        );
        assert_eq!(
            tree.object_span(2),
            Some((
                Span { start: 64, end: 120 },
                Some((obj_ref(9), Span { start: 4, end: 30 }))
            ))
        );
        tree.expand(tree.xref_folder);
        let xref_ids = tree.node(tree.xref_folder).children.clone();
        let labels: Vec<String> = xref_ids.iter().map(|id| tree.label(*id)).collect();
        assert_eq!(
            labels,
            vec!["xref table · 3 entries", "startxref → 120", "%%EOF"]
        );
    }

    #[test]
    fn expanding_objects_twice_requests_once() {
        let mut tree = TreeState::new((1, 7), 1);
        assert_eq!(tree.expand(tree.objects_folder), Some(TreeReq::Physical));
        assert_eq!(tree.expand(tree.objects_folder), None);
        assert_eq!(tree.expand(tree.xref_folder), None, "same physical pass");
    }

    #[test]
    fn logical_batch_builds_page_subtree() {
        let mut tree = TreeState::new((1, 7), 1);
        assert_eq!(tree.expand(tree.pages_folder), Some(TreeReq::Logical));
        tree.apply_batch(TreeReq::Logical, &logical_batch(), true);
        assert_eq!(tree.logical, LoadState::Loaded);
        let page_id = tree.node(tree.pages_folder).children[0];
        assert_eq!(tree.label(page_id), "Page 1");
        assert_eq!(tree.page_ref(0), Some(obj_ref(3)));
        tree.expand(page_id);
        let folder_ids = tree.node(page_id).children.clone();
        let labels: Vec<String> = folder_ids.iter().map(|id| tree.label(*id)).collect();
        assert_eq!(
            labels,
            vec!["Fonts (1)", "Images (1)", "Annotations (1)", "Contents"]
        );
        let font_id = tree.node(folder_ids[0]).children[0];
        assert_eq!(tree.label(font_id), "Helvetica · 5 0");
        let image_id = tree.node(folder_ids[1]).children[0];
        assert_eq!(tree.label(image_id), "32x16 · 7 0");
        let annot_id = tree.node(folder_ids[2]).children[0];
        assert_eq!(tree.label(annot_id), "Link · 8 0");
        assert_eq!(tree.page_of(font_id), Some(0));
    }

    #[test]
    fn contents_folder_requests_and_fills() {
        let mut tree = TreeState::new((1, 7), 1);
        tree.expand(tree.pages_folder);
        tree.apply_batch(TreeReq::Logical, &logical_batch(), true);
        let page_id = tree.node(tree.pages_folder).children[0];
        tree.expand(page_id);
        let contents_id = tree.node(page_id).children[3];
        assert_eq!(
            tree.expand(contents_id),
            Some(TreeReq::Contents { page: 0 })
        );
        assert_eq!(tree.expand(contents_id), None, "already loading");
        tree.apply_contents(0, &[obj_ref(4)]);
        assert_eq!(tree.label(contents_id), "Contents (1)");
        let stream_id = tree.node(contents_id).children[0];
        assert_eq!(tree.label(stream_id), "stream 4 0");
        assert_eq!(tree.selection_ref(stream_id), Some(obj_ref(4)));
    }

    #[test]
    fn selection_moves_over_visible_rows_only() {
        let mut tree = TreeState::new((1, 7), 1);
        tree.apply_batch(TreeReq::Physical, &physical_batch(), true);
        assert_eq!(tree.selected, tree.root);
        tree.select_next();
        assert_eq!(tree.selected, tree.pages_folder);
        tree.select_next();
        assert_eq!(tree.selected, tree.objects_folder);
        // Objects is collapsed: its children are not visited.
        tree.select_next();
        assert_eq!(tree.selected, tree.xref_folder);
        tree.select_next();
        assert_eq!(tree.selected, tree.trailer_node);
        tree.select_next();
        assert_eq!(tree.selected, tree.trailer_node, "clamped at bottom");
        tree.select_top();
        assert_eq!(tree.selected, tree.root);
        tree.select_bottom();
        assert_eq!(tree.selected, tree.trailer_node);
        tree.select_prev();
        assert_eq!(tree.selected, tree.xref_folder);
    }

    #[test]
    fn collapse_or_parent_folds_then_climbs() {
        let mut tree = TreeState::new((1, 7), 1);
        tree.apply_batch(TreeReq::Physical, &physical_batch(), true);
        tree.expand(tree.objects_folder);
        let first_object = tree.node(tree.objects_folder).children[0];
        tree.selected = first_object;
        assert!(tree.collapse_or_parent(first_object), "leaf climbs to parent");
        assert_eq!(tree.selected, tree.objects_folder);
        assert!(tree.collapse_or_parent(tree.objects_folder), "folds open branch");
        assert!(!tree.node(tree.objects_folder).expanded);
    }

    #[test]
    fn find_object_and_reveal_expand_ancestors() {
        let mut tree = TreeState::new((1, 7), 1);
        tree.apply_batch(TreeReq::Physical, &physical_batch(), true);
        let id = tree.find_object(obj_ref(2)).expect("object 2 present");
        assert!(!tree.node(tree.objects_folder).expanded);
        tree.reveal(id);
        assert!(tree.node(tree.objects_folder).expanded);
        assert!(tree
            .visible_rows()
            .iter()
            .any(|row| row.id == id));
        assert_eq!(tree.find_object(obj_ref(42)), None);
    }

    #[test]
    fn breadcrumb_walks_short_labels() {
        let mut tree = TreeState::new((1, 7), 1);
        assert_eq!(tree.breadcrumb(), "/Document");
        tree.apply_batch(TreeReq::Physical, &physical_batch(), true);
        let id = tree.find_object(obj_ref(1)).expect("object 1");
        tree.selected = id;
        assert_eq!(tree.breadcrumb(), "/Document/Objects/obj 1 0");
        tree.selected = tree.trailer_node;
        assert_eq!(tree.breadcrumb(), "/Document/Trailer");
    }

    #[test]
    fn mark_failed_records_failure() {
        let mut tree = TreeState::new((1, 7), 1);
        tree.expand(tree.objects_folder);
        tree.mark_failed(TreeReq::Physical);
        assert_eq!(tree.physical, LoadState::Failed);
    }
}
```

- [ ] Add `pub mod tree;` to `crates/pdfboss-tui/src/lib.rs` (below the crate docs).
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui tree` — expect compile errors (`cannot find type TreeState`, `cannot find TreeReq`, …): the failing state of TDD.
- [ ] Implement. Prepend to `crates/pdfboss-tui/src/tree.rs` (above the test module) the full implementation:

```rust
//! Tree pane: lazy hierarchy over a document's elements.
//!
//! Document → Pages (per page: Fonts, Images, Annotations, Contents) →
//! Objects (flat, by number) → Xref sections → Trailer. Sections populate
//! from element batches streamed by background tasks; `expand` reports
//! which load pass a first expansion needs.

use std::collections::HashMap;

use pdfboss_core::elements::{Element, Span, XrefKind};
use pdfboss_core::{Dict, Name, ObjRef};

/// Index into [`TreeState::nodes`].
pub type NodeId = usize;

/// Which lazily loaded data a tree section needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TreeReq {
    /// Physical pass: objects, xref sections, trailer, header, startxref, eof.
    Physical,
    /// Logical pass: pages with their fonts, images and annotations.
    Logical,
    /// One page's `/Contents` refs.
    Contents { page: usize },
}

/// Load progress of a lazily populated section.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadState {
    NotLoaded,
    Loading,
    Loaded,
    Failed,
}

/// What a tree node represents.
#[derive(Clone, PartialEq, Debug)]
pub enum NodeKind {
    Document,
    PagesFolder,
    Page { index: usize, r: ObjRef },
    FontsFolder { page: usize },
    Font { r: ObjRef, subtype: Name, base_font: Option<Name> },
    ImagesFolder { page: usize },
    Image { r: ObjRef, width: u32, height: u32 },
    AnnotationsFolder { page: usize },
    Annotation { r: ObjRef, subtype: Name },
    ContentsFolder { page: usize },
    ContentsStream { r: ObjRef },
    ObjectsFolder,
    Object { r: ObjRef, span: Span, in_objstm: Option<(ObjRef, Span)> },
    XrefFolder,
    XrefSection { kind: XrefKind, span: Span, entries: usize },
    StartXref { offset: u64, span: Span },
    Eof { span: Span },
    Trailer,
}

/// One node of the arena tree.
#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub kind: NodeKind,
    pub children: Vec<NodeId>,
    pub expanded: bool,
    pub load: LoadState,
}

/// A visible row: node plus indentation depth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TreeRow {
    pub id: NodeId,
    pub depth: usize,
}

/// Per-page subfolder ids, filled while applying the logical batch.
#[derive(Clone, Copy, Debug)]
struct PageFolders {
    page_node: NodeId,
    fonts: NodeId,
    images: NodeId,
    annotations: NodeId,
    contents: NodeId,
}

/// The whole tree pane model.
pub struct TreeState {
    pub nodes: Vec<Node>,
    pub selected: NodeId,
    pub scroll: usize,
    pub page_count: usize,
    pub version: (u8, u8),
    pub physical: LoadState,
    pub logical: LoadState,
    pub header_span: Option<Span>,
    pub trailer_dict: Option<Dict>,
    pub trailer_span: Option<Span>,
    pub root: NodeId,
    pub pages_folder: NodeId,
    pub objects_folder: NodeId,
    pub xref_folder: NodeId,
    pub trailer_node: NodeId,
    object_spans: HashMap<u32, (Span, Option<(ObjRef, Span)>)>,
    page_folders: HashMap<usize, PageFolders>,
}

impl TreeState {
    pub fn new(version: (u8, u8), page_count: usize) -> TreeState {
        let mut tree = TreeState {
            nodes: Vec::new(),
            selected: 0,
            scroll: 0,
            page_count,
            version,
            physical: LoadState::NotLoaded,
            logical: LoadState::NotLoaded,
            header_span: None,
            trailer_dict: None,
            trailer_span: None,
            root: 0,
            pages_folder: 0,
            objects_folder: 0,
            xref_folder: 0,
            trailer_node: 0,
            object_spans: HashMap::new(),
            page_folders: HashMap::new(),
        };
        tree.root = tree.add(None, NodeKind::Document);
        tree.nodes[tree.root].expanded = true;
        tree.pages_folder = tree.add(Some(tree.root), NodeKind::PagesFolder);
        tree.objects_folder = tree.add(Some(tree.root), NodeKind::ObjectsFolder);
        tree.xref_folder = tree.add(Some(tree.root), NodeKind::XrefFolder);
        tree.trailer_node = tree.add(Some(tree.root), NodeKind::Trailer);
        tree.selected = tree.root;
        tree
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    fn add(&mut self, parent: Option<NodeId>, kind: NodeKind) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            parent,
            kind,
            children: Vec::new(),
            expanded: false,
            load: LoadState::NotLoaded,
        });
        if let Some(parent_id) = parent {
            self.nodes[parent_id].children.push(id);
        }
        id
    }

    /// Whether a node can have children (shows an expansion glyph).
    pub fn is_branch(&self, id: NodeId) -> bool {
        matches!(
            self.nodes[id].kind,
            NodeKind::Document
                | NodeKind::PagesFolder
                | NodeKind::Page { .. }
                | NodeKind::FontsFolder { .. }
                | NodeKind::ImagesFolder { .. }
                | NodeKind::AnnotationsFolder { .. }
                | NodeKind::ContentsFolder { .. }
                | NodeKind::ObjectsFolder
                | NodeKind::XrefFolder
        )
    }

    /// Expands a branch node. Returns the load request the expansion needs
    /// when its data has not been requested yet (and marks it Loading).
    pub fn expand(&mut self, id: NodeId) -> Option<TreeReq> {
        if !self.is_branch(id) {
            return None;
        }
        self.nodes[id].expanded = true;
        let kind = self.nodes[id].kind.clone();
        match kind {
            NodeKind::PagesFolder if self.logical == LoadState::NotLoaded => {
                self.logical = LoadState::Loading;
                Some(TreeReq::Logical)
            }
            NodeKind::ObjectsFolder | NodeKind::XrefFolder
                if self.physical == LoadState::NotLoaded =>
            {
                self.physical = LoadState::Loading;
                Some(TreeReq::Physical)
            }
            NodeKind::ContentsFolder { page }
                if self.nodes[id].load == LoadState::NotLoaded =>
            {
                self.nodes[id].load = LoadState::Loading;
                Some(TreeReq::Contents { page })
            }
            NodeKind::Document
            | NodeKind::PagesFolder
            | NodeKind::Page { .. }
            | NodeKind::FontsFolder { .. }
            | NodeKind::ImagesFolder { .. }
            | NodeKind::AnnotationsFolder { .. }
            | NodeKind::ContentsFolder { .. }
            | NodeKind::ObjectsFolder
            | NodeKind::XrefFolder => None,
            NodeKind::Font { .. }
            | NodeKind::Image { .. }
            | NodeKind::Annotation { .. }
            | NodeKind::ContentsStream { .. }
            | NodeKind::Object { .. }
            | NodeKind::XrefSection { .. }
            | NodeKind::StartXref { .. }
            | NodeKind::Eof { .. }
            | NodeKind::Trailer => None,
        }
    }

    /// Collapses an expanded branch; on a leaf or collapsed node, moves the
    /// selection to the parent instead. Returns true when anything changed.
    pub fn collapse_or_parent(&mut self, id: NodeId) -> bool {
        if self.is_branch(id) && self.nodes[id].expanded {
            self.nodes[id].expanded = false;
            return true;
        }
        match self.nodes[id].parent {
            Some(parent_id) => {
                self.selected = parent_id;
                true
            }
            None => false,
        }
    }

    /// Applies a streamed element batch to the section `req` covers.
    pub fn apply_batch(&mut self, req: TreeReq, elements: &[Element], done: bool) {
        for element in elements {
            match element {
                Element::Header { version, span } => {
                    self.version = *version;
                    self.header_span = Some(*span);
                }
                // The parsed object value is not retained (`..`): the
                // inspector re-fetches on selection, keeping the tree small.
                Element::IndirectObject {
                    r,
                    span,
                    in_objstm,
                    ..
                } => {
                    self.object_spans.insert(r.num, (*span, *in_objstm));
                    self.add(
                        Some(self.objects_folder),
                        NodeKind::Object {
                            r: *r,
                            span: *span,
                            in_objstm: *in_objstm,
                        },
                    );
                }
                // Sections stream in xref-chain order (newest → oldest)
                // and are displayed as received.
                Element::XrefSection { kind, span, entries } => {
                    self.add(
                        Some(self.xref_folder),
                        NodeKind::XrefSection {
                            kind: *kind,
                            span: *span,
                            entries: *entries,
                        },
                    );
                }
                // Exactly one Trailer element per document (the merged
                // dict), so plain assignment is correct here.
                Element::Trailer { dict, span } => {
                    self.trailer_dict = Some(dict.clone());
                    self.trailer_span = Some(*span);
                }
                Element::StartXref { offset, span } => {
                    self.add(
                        Some(self.xref_folder),
                        NodeKind::StartXref {
                            offset: *offset,
                            span: *span,
                        },
                    );
                }
                Element::Eof { span } => {
                    self.add(Some(self.xref_folder), NodeKind::Eof { span: *span });
                }
                Element::Page { index, r } => {
                    let page_node = self.add(
                        Some(self.pages_folder),
                        NodeKind::Page { index: *index, r: *r },
                    );
                    let fonts =
                        self.add(Some(page_node), NodeKind::FontsFolder { page: *index });
                    let images =
                        self.add(Some(page_node), NodeKind::ImagesFolder { page: *index });
                    let annotations = self.add(
                        Some(page_node),
                        NodeKind::AnnotationsFolder { page: *index },
                    );
                    let contents = self.add(
                        Some(page_node),
                        NodeKind::ContentsFolder { page: *index },
                    );
                    self.page_folders.insert(
                        *index,
                        PageFolders {
                            page_node,
                            fonts,
                            images,
                            annotations,
                            contents,
                        },
                    );
                }
                Element::Font {
                    page: Some(page),
                    r,
                    subtype,
                    base_font,
                } => {
                    if let Some(folders) = self.page_folders.get(page).copied() {
                        self.add(
                            Some(folders.fonts),
                            NodeKind::Font {
                                r: *r,
                                subtype: subtype.clone(),
                                base_font: base_font.clone(),
                            },
                        );
                    }
                }
                Element::Image {
                    page: Some(page),
                    r,
                    width,
                    height,
                } => {
                    if let Some(folders) = self.page_folders.get(page).copied() {
                        self.add(
                            Some(folders.images),
                            NodeKind::Image {
                                r: *r,
                                width: *width,
                                height: *height,
                            },
                        );
                    }
                }
                Element::Annotation { page, r, subtype } => {
                    if let Some(folders) = self.page_folders.get(page).copied() {
                        self.add(
                            Some(folders.annotations),
                            NodeKind::Annotation {
                                r: *r,
                                subtype: subtype.clone(),
                            },
                        );
                    }
                }
                // Document-level fonts/images (page: None) stay reachable
                // through Objects; content ops are never streamed here.
                Element::Font { page: None, .. }
                | Element::Image { page: None, .. }
                | Element::ContentOp { .. } => {}
            }
        }
        if done {
            match req {
                TreeReq::Physical => self.physical = LoadState::Loaded,
                TreeReq::Logical => {
                    self.logical = LoadState::Loaded;
                    let loaded: Vec<NodeId> = self
                        .page_folders
                        .values()
                        .flat_map(|folders| {
                            [folders.fonts, folders.images, folders.annotations]
                        })
                        .collect();
                    for id in loaded {
                        self.nodes[id].load = LoadState::Loaded;
                    }
                }
                TreeReq::Contents { .. } => {}
            }
        }
    }

    /// Fills a page's Contents folder with its stream refs.
    pub fn apply_contents(&mut self, page: usize, refs: &[ObjRef]) {
        let Some(folders) = self.page_folders.get(&page).copied() else {
            return;
        };
        for r in refs {
            self.add(Some(folders.contents), NodeKind::ContentsStream { r: *r });
        }
        self.nodes[folders.contents].load = LoadState::Loaded;
    }

    /// Records a failed load pass.
    pub fn mark_failed(&mut self, req: TreeReq) {
        match req {
            TreeReq::Physical => self.physical = LoadState::Failed,
            TreeReq::Logical => self.logical = LoadState::Failed,
            TreeReq::Contents { page } => {
                if let Some(folders) = self.page_folders.get(&page).copied() {
                    self.nodes[folders.contents].load = LoadState::Failed;
                }
            }
        }
    }

    /// Depth-first walk of expanded nodes.
    pub fn visible_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.walk(self.root, 0, &mut rows);
        rows
    }

    fn walk(&self, id: NodeId, depth: usize, rows: &mut Vec<TreeRow>) {
        rows.push(TreeRow { id, depth });
        if self.nodes[id].expanded {
            for child in self.nodes[id].children.clone() {
                self.walk(child, depth + 1, rows);
            }
        }
    }

    /// Display label for a node (deterministic; used by snapshots).
    pub fn label(&self, id: NodeId) -> String {
        match &self.nodes[id].kind {
            NodeKind::Document => {
                format!("Document · PDF {}.{}", self.version.0, self.version.1)
            }
            NodeKind::PagesFolder => format!("Pages ({})", self.page_count),
            NodeKind::Page { index, .. } => format!("Page {}", index + 1),
            NodeKind::FontsFolder { .. } => {
                self.folder_label("Fonts", id)
            }
            NodeKind::Font { r, subtype, base_font } => {
                let face = base_font.as_ref().unwrap_or(subtype);
                format!("{} · {} {}", face.0, r.num, r.gen)
            }
            NodeKind::ImagesFolder { .. } => self.folder_label("Images", id),
            NodeKind::Image { r, width, height } => {
                format!("{}x{} · {} {}", width, height, r.num, r.gen)
            }
            NodeKind::AnnotationsFolder { .. } => self.folder_label("Annotations", id),
            NodeKind::Annotation { r, subtype } => {
                format!("{} · {} {}", subtype.0, r.num, r.gen)
            }
            NodeKind::ContentsFolder { .. } => self.folder_label("Contents", id),
            NodeKind::ContentsStream { r } => format!("stream {} {}", r.num, r.gen),
            NodeKind::ObjectsFolder => match self.physical {
                LoadState::Loaded => {
                    format!("Objects ({})", self.nodes[id].children.len())
                }
                LoadState::NotLoaded | LoadState::Loading | LoadState::Failed => {
                    "Objects".to_string()
                }
            },
            NodeKind::Object { r, .. } => format!("obj {} {}", r.num, r.gen),
            NodeKind::XrefFolder => match self.physical {
                LoadState::Loaded => {
                    let secs = self.nodes[id]
                        .children
                        .iter()
                        .filter(|child| {
                            matches!(self.nodes[**child].kind, NodeKind::XrefSection { .. })
                        })
                        .count();
                    format!("Xref ({} secs)", secs)
                }
                LoadState::NotLoaded | LoadState::Loading | LoadState::Failed => {
                    "Xref".to_string()
                }
            },
            NodeKind::XrefSection { kind, entries, .. } => match kind {
                XrefKind::Table => format!("xref table · {} entries", entries),
                XrefKind::Stream => format!("xref stream · {} entries", entries),
            },
            NodeKind::StartXref { offset, .. } => format!("startxref → {}", offset),
            NodeKind::Eof { .. } => "%%EOF".to_string(),
            NodeKind::Trailer => "Trailer".to_string(),
        }
    }

    fn folder_label(&self, name: &str, id: NodeId) -> String {
        match self.nodes[id].load {
            LoadState::Loaded => format!("{} ({})", name, self.nodes[id].children.len()),
            LoadState::NotLoaded | LoadState::Loading | LoadState::Failed => {
                name.to_string()
            }
        }
    }

    /// Short label for breadcrumbs (no counts, no versions).
    fn short_label(&self, id: NodeId) -> String {
        match &self.nodes[id].kind {
            NodeKind::Document => "Document".to_string(),
            NodeKind::PagesFolder => "Pages".to_string(),
            NodeKind::Page { index, .. } => format!("Page {}", index + 1),
            NodeKind::FontsFolder { .. } => "Fonts".to_string(),
            NodeKind::ImagesFolder { .. } => "Images".to_string(),
            NodeKind::AnnotationsFolder { .. } => "Annotations".to_string(),
            NodeKind::ContentsFolder { .. } => "Contents".to_string(),
            NodeKind::ObjectsFolder => "Objects".to_string(),
            NodeKind::XrefFolder => "Xref".to_string(),
            NodeKind::Trailer => "Trailer".to_string(),
            NodeKind::Font { .. }
            | NodeKind::Image { .. }
            | NodeKind::Annotation { .. }
            | NodeKind::ContentsStream { .. }
            | NodeKind::Object { .. }
            | NodeKind::XrefSection { .. }
            | NodeKind::StartXref { .. }
            | NodeKind::Eof { .. } => self.label(id),
        }
    }

    /// `/Document/Objects/obj 12 0`-style path of the current selection.
    pub fn breadcrumb(&self) -> String {
        let mut parts = Vec::new();
        let mut cursor = Some(self.selected);
        while let Some(id) = cursor {
            parts.push(self.short_label(id));
            cursor = self.nodes[id].parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// The object node for `r` (generation lenient), if loaded.
    pub fn find_object(&self, r: ObjRef) -> Option<NodeId> {
        self.nodes[self.objects_folder]
            .children
            .iter()
            .copied()
            .find(|id| match self.nodes[*id].kind {
                NodeKind::Object { r: node_ref, .. } => node_ref.num == r.num,
                NodeKind::Document
                | NodeKind::PagesFolder
                | NodeKind::Page { .. }
                | NodeKind::FontsFolder { .. }
                | NodeKind::Font { .. }
                | NodeKind::ImagesFolder { .. }
                | NodeKind::Image { .. }
                | NodeKind::AnnotationsFolder { .. }
                | NodeKind::Annotation { .. }
                | NodeKind::ContentsFolder { .. }
                | NodeKind::ContentsStream { .. }
                | NodeKind::ObjectsFolder
                | NodeKind::XrefFolder
                | NodeKind::XrefSection { .. }
                | NodeKind::StartXref { .. }
                | NodeKind::Eof { .. }
                | NodeKind::Trailer => false,
            })
    }

    /// Expands every ancestor so `id` becomes visible.
    pub fn reveal(&mut self, id: NodeId) {
        let mut cursor = self.nodes[id].parent;
        while let Some(parent_id) = cursor {
            self.nodes[parent_id].expanded = true;
            cursor = self.nodes[parent_id].parent;
        }
    }

    /// The object reference a node points at, if any.
    pub fn selection_ref(&self, id: NodeId) -> Option<ObjRef> {
        match self.nodes[id].kind {
            NodeKind::Page { r, .. }
            | NodeKind::Font { r, .. }
            | NodeKind::Image { r, .. }
            | NodeKind::Annotation { r, .. }
            | NodeKind::ContentsStream { r }
            | NodeKind::Object { r, .. } => Some(r),
            NodeKind::Document
            | NodeKind::PagesFolder
            | NodeKind::FontsFolder { .. }
            | NodeKind::ImagesFolder { .. }
            | NodeKind::AnnotationsFolder { .. }
            | NodeKind::ContentsFolder { .. }
            | NodeKind::ObjectsFolder
            | NodeKind::XrefFolder
            | NodeKind::XrefSection { .. }
            | NodeKind::StartXref { .. }
            | NodeKind::Eof { .. }
            | NodeKind::Trailer => None,
        }
    }

    /// The physical span (and objstm placement) recorded for object `num`.
    pub fn object_span(&self, num: u32) -> Option<(Span, Option<(ObjRef, Span)>)> {
        self.object_spans.get(&num).copied()
    }

    /// The page dictionary ref of page `page`, once the logical pass ran.
    pub fn page_ref(&self, page: usize) -> Option<ObjRef> {
        let folders = self.page_folders.get(&page)?;
        if let NodeKind::Page { r, .. } = self.nodes[folders.page_node].kind {
            Some(r)
        } else {
            None
        }
    }

    /// Nearest ancestor page index of a node (the node itself counts).
    pub fn page_of(&self, id: NodeId) -> Option<usize> {
        let mut cursor = Some(id);
        while let Some(node_id) = cursor {
            let page = match self.nodes[node_id].kind {
                NodeKind::Page { index, .. } => Some(index),
                NodeKind::FontsFolder { page }
                | NodeKind::ImagesFolder { page }
                | NodeKind::AnnotationsFolder { page }
                | NodeKind::ContentsFolder { page } => Some(page),
                NodeKind::Document
                | NodeKind::PagesFolder
                | NodeKind::Font { .. }
                | NodeKind::Image { .. }
                | NodeKind::Annotation { .. }
                | NodeKind::ContentsStream { .. }
                | NodeKind::ObjectsFolder
                | NodeKind::Object { .. }
                | NodeKind::XrefFolder
                | NodeKind::XrefSection { .. }
                | NodeKind::StartXref { .. }
                | NodeKind::Eof { .. }
                | NodeKind::Trailer => None,
            };
            if let Some(index) = page {
                return Some(index);
            }
            cursor = self.nodes[node_id].parent;
        }
        None
    }

    fn selected_position(&self, rows: &[TreeRow]) -> usize {
        rows.iter()
            .position(|row| row.id == self.selected)
            .unwrap_or(0)
    }

    pub fn select_next(&mut self) {
        let rows = self.visible_rows();
        let position = self.selected_position(&rows);
        if position + 1 < rows.len() {
            self.selected = rows[position + 1].id;
        }
    }

    pub fn select_prev(&mut self) {
        let rows = self.visible_rows();
        let position = self.selected_position(&rows);
        if position > 0 {
            self.selected = rows[position - 1].id;
        }
    }

    pub fn select_top(&mut self) {
        self.selected = self.root;
    }

    pub fn select_bottom(&mut self) {
        if let Some(row) = self.visible_rows().last() {
            self.selected = row.id;
        }
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui tree` — expect all 10 tests green.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy -p pdfboss-tui --all-targets -- -D warnings && CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check` — expect clean.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): lazy element tree state machine"`

---

### Task 3: Hex pane model and formatting (`hexview.rs`)

**Files:**
- Create: `crates/pdfboss-tui/src/hexview.rs`
- Modify: `crates/pdfboss-tui/src/lib.rs` (add module declaration)
- Test: unit tests inside `crates/pdfboss-tui/src/hexview.rs`

**Interfaces:**
- Consumes: `pdfboss_core::elements::Span` (plan 01), `pdfboss_core::ObjRef`, ratatui `Color`/`Style`/`Line`/`Span`(text).
- Produces: `BYTES_PER_LINE: usize = 8`, `WINDOW_BYTES: usize = 65536`, `ByteClass`, `byte_class(u8) -> ByteClass`, `class_color(ByteClass) -> Color`, `HexSource { File { span: Span }, DecodedObjStm { container: ObjRef } }`, `HexState` (+ `new/set_source/clear/apply_loaded/line_count/visible_window_missing/scroll_by/scroll_to/title`), `window_for_line(total_len: u64, line: u64) -> (u64, usize)`, `highlight_cols(line_off: u64, len: usize, highlight: Span) -> Option<(usize, usize)>`, `hex_line(abs_off: u64, bytes: &[u8], hl: Option<(usize, usize)>) -> ratatui::text::Line<'static>`.

**Steps:**

- [ ] Write the failing tests. Create `crates/pdfboss-tui/src/hexview.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::elements::Span;

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|part| part.content.as_ref()).collect()
    }

    #[test]
    fn byte_classes() {
        assert_eq!(byte_class(0x00), ByteClass::Null);
        assert_eq!(byte_class(b'A'), ByteClass::Printable);
        assert_eq!(byte_class(b' '), ByteClass::Printable);
        assert_eq!(byte_class(b'\n'), ByteClass::Whitespace);
        assert_eq!(byte_class(b'\t'), ByteClass::Whitespace);
        assert_eq!(byte_class(b'\r'), ByteClass::Whitespace);
        assert_eq!(byte_class(0xE2), ByteClass::Other);
    }

    #[test]
    fn hex_line_formats_full_and_short_rows() {
        let full = hex_line(0, b"%PDF-1.7", None);
        assert_eq!(
            line_text(&full),
            "00000000 \u{2502} 25 50 44 46 2d 31 2e 37 \u{2502} %PDF-1.7"
        );
        let short = hex_line(8, &[0x0a, 0x25, 0xe2, 0xe3, 0xcf, 0xd3, 0x0a], None);
        assert_eq!(
            line_text(&short),
            "00000008 \u{2502} 0a 25 e2 e3 cf d3 0a    \u{2502} \u{b7}%\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}"
        );
    }

    #[test]
    fn hex_line_reverses_highlighted_columns() {
        let line = hex_line(0, b"ABCDEFGH", Some((2, 5)));
        // Byte cells 2..5 and their ascii cells carry REVERSED style.
        let styled: Vec<(String, bool)> = line
            .spans
            .iter()
            .map(|part| {
                (
                    part.content.as_ref().to_string(),
                    part.style
                        .add_modifier
                        .contains(ratatui::style::Modifier::REVERSED),
                )
            })
            .collect();
        let reversed_text: String = styled
            .iter()
            .filter(|(_, on)| *on)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(reversed_text, "43 44 45 CDE");
    }

    #[test]
    fn window_math_covers_span_in_aligned_chunks() {
        assert_eq!(window_for_line(100, 0), (0, 100));
        assert_eq!(window_for_line(200_000, 0), (0, WINDOW_BYTES));
        // Line 8192 starts at byte 65536: second window.
        assert_eq!(window_for_line(200_000, 8192), (65536, WINDOW_BYTES));
        // Final window is short.
        assert_eq!(window_for_line(200_000, 24576), (196_608, 3392));
    }

    #[test]
    fn highlight_math_clamps_to_line() {
        let hl = Span { start: 10, end: 20 };
        assert_eq!(highlight_cols(0, 8, hl), None);
        assert_eq!(highlight_cols(8, 8, hl), Some((2, 8)));
        assert_eq!(highlight_cols(16, 8, hl), Some((0, 4)));
        assert_eq!(highlight_cols(24, 8, hl), None);
        assert_eq!(highlight_cols(8, 4, Span { start: 10, end: 11 }), Some((2, 3)));
    }

    #[test]
    fn state_scrolls_within_span_and_reports_missing_window() {
        let mut hex = HexState::new();
        hex.set_source(HexSource::File {
            span: Span { start: 0x10, end: 0x10 + 200_000 },
        });
        assert!(hex.loading);
        hex.apply_loaded(0, 200_000, vec![0u8; WINDOW_BYTES]);
        assert!(!hex.loading);
        assert_eq!(hex.line_count(), 25_000);
        hex.scroll_by(5);
        assert_eq!(hex.scroll_line, 5);
        assert_eq!(hex.visible_window_missing(7), None);
        hex.scroll_to(24_999);
        assert_eq!(hex.scroll_line, 24_999);
        assert_eq!(hex.visible_window_missing(7), Some(196_608));
        hex.scroll_by(-50_000);
        assert_eq!(hex.scroll_line, 0);
        assert_eq!(hex.title(), "Hex 0x10..0x30d50");
    }

    #[test]
    fn objstm_source_titles_and_holds_highlight() {
        let mut hex = HexState::new();
        hex.set_source(HexSource::DecodedObjStm {
            container: pdfboss_core::ObjRef { num: 9, gen: 0 },
        });
        hex.highlight = Some(Span { start: 4, end: 30 });
        hex.apply_loaded(0, 64, vec![0u8; 64]);
        assert_eq!(hex.title(), "Hex obj 9 0 decoded 0x0..0x40");
    }

    #[test]
    fn cleared_state_has_no_title_range() {
        let mut hex = HexState::new();
        hex.set_source(HexSource::File {
            span: Span { start: 0, end: 8 },
        });
        hex.clear();
        assert_eq!(hex.title(), "Hex");
        assert_eq!(hex.line_count(), 0);
    }
}
```

- [ ] Add `pub mod hexview;` to `crates/pdfboss-tui/src/lib.rs`.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui hexview` — expect compile errors (missing `HexState` etc.).
- [ ] Implement. Prepend to `crates/pdfboss-tui/src/hexview.rs`:

```rust
//! Hex pane: hexyl-style `offset │ hex │ ascii` lines with byte-class
//! colors, windowed fetching over a span, and objstm-member highlighting.

use pdfboss_core::elements::Span;
use pdfboss_core::ObjRef;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

/// Bytes shown per hex line (8 keeps lines inside a 35%-split 80-col pane).
pub const BYTES_PER_LINE: usize = 8;
/// Bytes fetched per window; spans larger than this stream on demand.
pub const WINDOW_BYTES: usize = 64 * 1024;

/// hexyl-style byte classes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ByteClass {
    Null,
    Printable,
    Whitespace,
    Other,
}

/// Classifies a byte for coloring.
pub fn byte_class(byte: u8) -> ByteClass {
    match byte {
        0x00 => ByteClass::Null,
        b'\t' | b'\n' | b'\x0c' | b'\r' => ByteClass::Whitespace,
        0x20..=0x7e => ByteClass::Printable,
        // 0x0b (vertical tab) and everything non-ascii-printable.
        0x01..=0x1f | 0x7f..=0xff => ByteClass::Other,
    }
}

/// Color per byte class.
pub fn class_color(class: ByteClass) -> Color {
    match class {
        ByteClass::Null => Color::DarkGray,
        ByteClass::Printable => Color::Cyan,
        ByteClass::Whitespace => Color::Green,
        ByteClass::Other => Color::Yellow,
    }
}

/// Where the hex pane's bytes come from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HexSource {
    /// A byte range of the physical file; offsets shown are absolute.
    File { span: Span },
    /// The decoded bytes of an object-stream container; offsets shown are
    /// relative to the decoded buffer (a member's range is highlighted).
    DecodedObjStm { container: ObjRef },
}

/// Hex pane model. `scroll_line` addresses the whole source in
/// [`BYTES_PER_LINE`]-byte lines; only one [`WINDOW_BYTES`] window of
/// bytes is resident at a time.
pub struct HexState {
    pub source: Option<HexSource>,
    /// Total viewable length: span length (File) or decoded length (ObjStm).
    pub total_len: u64,
    /// Absolute display offset of relative offset 0 (span.start for File).
    pub base: u64,
    /// Relative offset of `bytes[0]` within the source.
    pub window_start: u64,
    pub bytes: Vec<u8>,
    pub scroll_line: u64,
    /// Highlighted byte range, relative to the source (objstm members).
    pub highlight: Option<Span>,
    pub loading: bool,
    pub error: Option<String>,
}

impl HexState {
    pub fn new() -> HexState {
        HexState {
            source: None,
            total_len: 0,
            base: 0,
            window_start: 0,
            bytes: Vec::new(),
            scroll_line: 0,
            highlight: None,
            loading: false,
            error: None,
        }
    }

    /// Points the pane at a new source; bytes arrive via [`apply_loaded`].
    pub fn set_source(&mut self, source: HexSource) {
        self.total_len = match &source {
            HexSource::File { span } => span.end.saturating_sub(span.start),
            // Unknown until the container is decoded.
            HexSource::DecodedObjStm { .. } => 0,
        };
        self.base = match &source {
            HexSource::File { span } => span.start,
            HexSource::DecodedObjStm { .. } => 0,
        };
        self.source = Some(source);
        self.window_start = 0;
        self.bytes.clear();
        self.scroll_line = 0;
        self.highlight = None;
        self.loading = true;
        self.error = None;
    }

    /// Empties the pane (folder-ish selections have no bytes).
    pub fn clear(&mut self) {
        self.source = None;
        self.total_len = 0;
        self.base = 0;
        self.window_start = 0;
        self.bytes.clear();
        self.scroll_line = 0;
        self.highlight = None;
        self.loading = false;
        self.error = None;
    }

    /// Installs a loaded window.
    pub fn apply_loaded(&mut self, window_start: u64, total_len: u64, bytes: Vec<u8>) {
        self.window_start = window_start;
        self.total_len = total_len;
        self.bytes = bytes;
        self.loading = false;
        self.error = None;
    }

    /// Total number of hex lines in the source.
    pub fn line_count(&self) -> u64 {
        self.total_len.div_ceil(BYTES_PER_LINE as u64)
    }

    /// Scrolls by `delta` lines, clamped to the source.
    pub fn scroll_by(&mut self, delta: i64) {
        let target = if delta.is_negative() {
            self.scroll_line.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll_line.saturating_add(delta as u64)
        };
        self.scroll_to(target);
    }

    /// Scrolls to an absolute line, clamped to the last line.
    pub fn scroll_to(&mut self, line: u64) {
        let last = self.line_count().saturating_sub(1);
        self.scroll_line = line.min(last);
    }

    /// If the rows `scroll_line..scroll_line+visible` need bytes outside
    /// the resident window, returns the window start to fetch.
    pub fn visible_window_missing(&self, visible: u16) -> Option<u64> {
        if self.source.is_none() || self.total_len == 0 {
            return None;
        }
        let first_byte = self.scroll_line * BYTES_PER_LINE as u64;
        let last_byte = ((self.scroll_line + visible as u64) * BYTES_PER_LINE as u64)
            .min(self.total_len);
        let window_end = self.window_start + self.bytes.len() as u64;
        if first_byte >= self.window_start && last_byte <= window_end {
            return None;
        }
        Some(window_for_line(self.total_len, self.scroll_line).0)
    }

    /// Pane title: source and viewed range.
    pub fn title(&self) -> String {
        match &self.source {
            None => "Hex".to_string(),
            Some(HexSource::File { span }) => {
                format!("Hex {:#x}..{:#x}", span.start, span.end)
            }
            Some(HexSource::DecodedObjStm { container }) => {
                if self.total_len == 0 {
                    format!("Hex obj {} {} decoded", container.num, container.gen)
                } else {
                    format!(
                        "Hex obj {} {} decoded {:#x}..{:#x}",
                        container.num, container.gen, 0, self.total_len
                    )
                }
            }
        }
    }
}

impl Default for HexState {
    fn default() -> HexState {
        HexState::new()
    }
}

/// The [`WINDOW_BYTES`]-aligned window (relative start, length) containing
/// 8-byte line `line` of a `total_len`-byte source.
pub fn window_for_line(total_len: u64, line: u64) -> (u64, usize) {
    let byte = line * BYTES_PER_LINE as u64;
    let start = (byte / WINDOW_BYTES as u64) * WINDOW_BYTES as u64;
    let len = (total_len - start.min(total_len)).min(WINDOW_BYTES as u64) as usize;
    (start, len)
}

/// Columns `(first, end_exclusive)` of a line starting at relative offset
/// `line_off` with `len` bytes that fall inside `highlight`.
pub fn highlight_cols(line_off: u64, len: usize, highlight: Span) -> Option<(usize, usize)> {
    let line_end = line_off + len as u64;
    let start = highlight.start.max(line_off);
    let end = highlight.end.min(line_end);
    if start >= end {
        return None;
    }
    Some(((start - line_off) as usize, (end - line_off) as usize))
}

/// Renders one hex line: `AAAAAAAA │ xx xx … │ ascii`, byte-class colored,
/// with columns in `hl` shown REVERSED.
pub fn hex_line(abs_off: u64, bytes: &[u8], hl: Option<(usize, usize)>) -> Line<'static> {
    let mut parts: Vec<ratatui::text::Span<'static>> = Vec::new();
    parts.push(ratatui::text::Span::styled(
        format!("{:08x} \u{2502} ", abs_off),
        Style::default().fg(Color::DarkGray),
    ));
    let highlighted = |column: usize| -> bool {
        hl.is_some_and(|(first, end)| column >= first && column < end)
    };
    for column in 0..BYTES_PER_LINE {
        match bytes.get(column) {
            Some(byte) => {
                let mut style = Style::default().fg(class_color(byte_class(*byte)));
                if highlighted(column) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                parts.push(ratatui::text::Span::styled(format!("{:02x} ", byte), style));
            }
            None => parts.push(ratatui::text::Span::raw("   ")),
        }
    }
    parts.push(ratatui::text::Span::styled(
        "\u{2502} ".to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    for (column, byte) in bytes.iter().enumerate() {
        let symbol = if (0x20..=0x7e).contains(byte) {
            char::from(*byte).to_string()
        } else {
            "\u{b7}".to_string()
        };
        let mut style = Style::default().fg(class_color(byte_class(*byte)));
        if highlighted(column) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        parts.push(ratatui::text::Span::styled(symbol, style));
    }
    Line::from(parts)
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui hexview` — expect all 8 tests green. Note: in the highlight test, byte values of `"ABCDEFGH"` are 0x41..0x48, so columns 2..5 render `"43 44 45 "` — the trailing space of each hex cell is part of the styled span; the expected reversed text `"43 44 45 CDE"` accounts for it.
- [ ] Run clippy + fmt as in Task 2 — expect clean.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): hexyl-style hex pane model with windowed spans"`

---

### Task 4: Search model (`search.rs`)

**Files:**
- Create: `crates/pdfboss-tui/src/search.rs`
- Modify: `crates/pdfboss-tui/src/lib.rs` (add module declaration)
- Test: unit tests inside `crates/pdfboss-tui/src/search.rs`

**Interfaces:**
- Consumes: `pdfboss_core::{Dict, Name, Object, ObjRef}`.
- Produces: `SearchHit { r: ObjRef }`, `SearchState` (+ `new/open/cancel/accept/push_char/pop_char/add_hit/finish/next_hit/prev_hit/status_line`), `object_matches(query: &str, num: u32, object: &Object) -> bool`.

**Steps:**

- [ ] Write the failing tests. Create `crates/pdfboss-tui/src/search.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{Dict, Name, Object, ObjRef};

    fn page_dict() -> Object {
        let mut dict = Dict::new();
        dict.insert(Name("Type".to_string()), Object::Name(Name("Page".to_string())));
        dict.insert(
            Name("Contents".to_string()),
            Object::Ref(ObjRef { num: 13, gen: 0 }),
        );
        dict.insert(
            Name("Note".to_string()),
            Object::String(b"Hello World".to_vec()),
        );
        Object::Dict(dict)
    }

    #[test]
    fn matches_object_number_keys_names_and_strings() {
        let object = page_dict();
        assert!(object_matches("12", 12, &object), "object number");
        assert!(object_matches("contents", 12, &object), "dict key");
        assert!(object_matches("page", 12, &object), "name value");
        assert!(object_matches("hello w", 12, &object), "string content");
        assert!(object_matches("PAGE", 12, &object), "case-insensitive");
        assert!(!object_matches("zebra", 12, &object));
    }

    #[test]
    fn matches_nested_arrays_and_stream_dicts() {
        let inner = page_dict();
        let object = Object::Array(vec![Object::Int(7), inner]);
        assert!(object_matches("hello", 3, &object));
        let mut stream_dict = Dict::new();
        stream_dict.insert(
            Name("Filter".to_string()),
            Object::Name(Name("FlateDecode".to_string())),
        );
        let stream = Object::Stream(pdfboss_core::Stream {
            dict: stream_dict,
            data: Vec::new(),
        });
        assert!(object_matches("flate", 3, &stream));
    }

    #[test]
    fn generation_bumps_invalidate_stale_hits() {
        let mut search = SearchState::new();
        search.open();
        let first = search.push_char('a');
        assert!(search.add_hit(first, SearchHit { r: ObjRef { num: 1, gen: 0 } }));
        let second = search.push_char('b');
        assert_ne!(first, second);
        assert!(!search.add_hit(first, SearchHit { r: ObjRef { num: 2, gen: 0 } }));
        assert_eq!(search.hits.len(), 0, "new keystroke cleared old hits");
        assert!(search.add_hit(second, SearchHit { r: ObjRef { num: 3, gen: 0 } }));
        assert!(search.running);
        search.finish(second);
        assert!(!search.running);
    }

    #[test]
    fn next_and_prev_wrap_over_hits() {
        let mut search = SearchState::new();
        search.open();
        let generation = search.push_char('x');
        for num in [4u32, 8, 15] {
            search.add_hit(generation, SearchHit { r: ObjRef { num, gen: 0 } });
        }
        assert_eq!(search.next_hit().map(|hit| hit.r.num), Some(4));
        assert_eq!(search.next_hit().map(|hit| hit.r.num), Some(8));
        assert_eq!(search.next_hit().map(|hit| hit.r.num), Some(15));
        assert_eq!(search.next_hit().map(|hit| hit.r.num), Some(4), "wraps");
        assert_eq!(search.prev_hit().map(|hit| hit.r.num), Some(15), "wraps back");
    }

    #[test]
    fn pop_char_and_cancel() {
        let mut search = SearchState::new();
        search.open();
        assert!(search.active);
        search.push_char('a');
        search.push_char('b');
        assert_eq!(search.pop_char(), Some(3), "third generation");
        assert_eq!(search.query, "a");
        search.pop_char();
        assert_eq!(search.pop_char(), None, "empty query pops nothing");
        search.cancel();
        assert!(!search.active);
        assert!(search.query.is_empty());
        assert!(search.hits.is_empty());
    }

    #[test]
    fn status_line_reports_query_and_hits() {
        let mut search = SearchState::new();
        search.open();
        let generation = search.push_char('p');
        search.add_hit(generation, SearchHit { r: ObjRef { num: 3, gen: 0 } });
        assert_eq!(search.status_line(), "/p \u{b7} 1 hits \u{2026}");
        search.finish(generation);
        assert_eq!(search.status_line(), "/p \u{b7} 1 hits");
    }
}
```

- [ ] Add `pub mod search;` to `crates/pdfboss-tui/src/lib.rs`.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui search` — expect compile errors (missing `SearchState` etc.).
- [ ] Implement. Prepend to `crates/pdfboss-tui/src/search.rs`:

```rust
//! Incremental object search: case-insensitive matching over object
//! numbers, dict keys, name values and string contents. Results stream in
//! from a background task tagged with a generation; stale generations are
//! dropped here.

use pdfboss_core::{Object, ObjRef};

/// One search result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SearchHit {
    pub r: ObjRef,
}

/// Search bar + result-set model.
pub struct SearchState {
    /// Whether the status-bar input is open (keystrokes edit the query).
    pub active: bool,
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub cursor: Option<usize>,
    pub generation: u64,
    /// Whether a background task is still streaming results.
    pub running: bool,
}

impl SearchState {
    pub fn new() -> SearchState {
        SearchState {
            active: false,
            query: String::new(),
            hits: Vec::new(),
            cursor: None,
            generation: 0,
            running: false,
        }
    }

    /// Opens the input (`/`).
    pub fn open(&mut self) {
        self.active = true;
        self.query.clear();
        self.hits.clear();
        self.cursor = None;
        self.running = false;
    }

    /// Closes the input and discards everything (Esc).
    pub fn cancel(&mut self) {
        self.active = false;
        self.query.clear();
        self.hits.clear();
        self.cursor = None;
        self.running = false;
    }

    /// Closes the input keeping hits for n/N navigation (Enter).
    pub fn accept(&mut self) {
        self.active = false;
    }

    /// Appends a character; returns the new generation to search under.
    pub fn push_char(&mut self, c: char) -> u64 {
        self.query.push(c);
        self.restart()
    }

    /// Removes the last character; `None` when the query was empty.
    pub fn pop_char(&mut self) -> Option<u64> {
        self.query.pop()?;
        Some(self.restart())
    }

    fn restart(&mut self) -> u64 {
        self.generation += 1;
        self.hits.clear();
        self.cursor = None;
        self.running = !self.query.is_empty();
        self.generation
    }

    /// Adds a hit if it belongs to the current generation.
    pub fn add_hit(&mut self, generation: u64, hit: SearchHit) -> bool {
        if generation != self.generation {
            return false;
        }
        self.hits.push(hit);
        true
    }

    /// Marks the current generation's task finished.
    pub fn finish(&mut self, generation: u64) {
        if generation == self.generation {
            self.running = false;
        }
    }

    /// Advances to the next hit, wrapping.
    pub fn next_hit(&mut self) -> Option<SearchHit> {
        if self.hits.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => 0,
            Some(index) => (index + 1) % self.hits.len(),
        };
        self.cursor = Some(next);
        Some(self.hits[next])
    }

    /// Steps back to the previous hit, wrapping.
    pub fn prev_hit(&mut self) -> Option<SearchHit> {
        if self.hits.is_empty() {
            return None;
        }
        let prev = match self.cursor {
            None => self.hits.len() - 1,
            Some(0) => self.hits.len() - 1,
            Some(index) => index - 1,
        };
        self.cursor = Some(prev);
        Some(self.hits[prev])
    }

    /// Status-bar text while the input is open.
    pub fn status_line(&self) -> String {
        let running = if self.running { " \u{2026}" } else { "" };
        format!("/{} \u{b7} {} hits{}", self.query, self.hits.len(), running)
    }
}

impl Default for SearchState {
    fn default() -> SearchState {
        SearchState::new()
    }
}

/// Case-insensitive match of `query` against an object's number, dict keys,
/// name values and string contents (recursing through arrays, dicts and
/// stream dicts).
pub fn object_matches(query: &str, num: u32, object: &Object) -> bool {
    let needle = query.to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }
    if num.to_string().contains(&needle) {
        return true;
    }
    value_matches(&needle, object)
}

fn value_matches(needle: &str, object: &Object) -> bool {
    match object {
        Object::Name(name) => name.0.to_ascii_lowercase().contains(needle),
        Object::String(bytes) => String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains(needle),
        Object::Array(items) => items.iter().any(|item| value_matches(needle, item)),
        Object::Dict(dict) => dict.iter().any(|(key, value)| {
            key.0.to_ascii_lowercase().contains(needle) || value_matches(needle, value)
        }),
        Object::Stream(stream) => stream.dict.iter().any(|(key, value)| {
            key.0.to_ascii_lowercase().contains(needle) || value_matches(needle, value)
        }),
        Object::Null
        | Object::Bool(..)
        | Object::Int(..)
        | Object::Real(..)
        | Object::Ref(..) => false,
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui search` — expect all 6 tests green.
- [ ] Run clippy + fmt — expect clean.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): incremental search model with generation-tagged hits"`

---

### Task 5: Inspector model (`inspector.rs`)

**Files:**
- Create: `crates/pdfboss-tui/src/inspector.rs`
- Modify: `crates/pdfboss-tui/src/lib.rs` (add module declaration)
- Test: unit tests inside `crates/pdfboss-tui/src/inspector.rs`

**Interfaces:**
- Consumes: `pdfboss_core::pretty::{format_object, format_dict}` (plan 01: moved from the CLI unchanged — `pub fn format_object(obj: &Object) -> String`, `pub fn format_dict(dict: &Dict) -> String`); `pdfboss_core::content::parse_content(data: &[u8]) -> Result<Vec<Op>>` (existing, `crates/pdfboss-core/src/content.rs:207`); `pdfboss_core::{Object, ObjRef}`.
- Produces: `InspectorMode`, `InspectorPayload`, `InspectorState` (+ `new/show_message/show_loading/set_object/set_decoded/is_stream/cycle_mode/move_cursor/current_ref/mode_name`), `ref_lines(text: &str) -> Vec<(usize, ObjRef)>`, `bytes_lines(data: &[u8]) -> Vec<String>`, `ops_lines(data: &[u8]) -> Vec<String>`.

**Steps:**

- [ ] Write the failing tests. Create `crates/pdfboss-tui/src/inspector.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{Dict, Name, Object, ObjRef, Stream};

    fn catalog() -> Object {
        let mut dict = Dict::new();
        dict.insert(Name("Type".to_string()), Object::Name(Name("Catalog".to_string())));
        dict.insert(
            Name("Pages".to_string()),
            Object::Ref(ObjRef { num: 2, gen: 0 }),
        );
        Object::Dict(dict)
    }

    fn content_stream() -> Object {
        Object::Stream(Stream {
            dict: Dict::new(),
            data: b"raw-bytes".to_vec(),
        })
    }

    #[test]
    fn ref_lines_finds_references_with_line_numbers() {
        let text = "<<\n  /Pages 2 0 R\n  /Other [3 1 R 4 0 R]\n>>";
        assert_eq!(
            ref_lines(text),
            vec![
                (1, ObjRef { num: 2, gen: 0 }),
                (2, ObjRef { num: 3, gen: 1 }),
                (2, ObjRef { num: 4, gen: 0 }),
            ]
        );
        assert_eq!(ref_lines("no refs 12 here"), Vec::new());
    }

    #[test]
    fn set_object_builds_pretty_lines_and_refs() {
        let mut inspector = InspectorState::new();
        inspector.set_object(ObjRef { num: 1, gen: 0 }, catalog());
        assert_eq!(inspector.title, "obj 1 0");
        assert_eq!(
            inspector.lines,
            vec!["<<", "  /Pages 2 0 R", "  /Type /Catalog", ">>"]
        );
        assert_eq!(inspector.refs, vec![(1, ObjRef { num: 2, gen: 0 })]);
        assert!(!inspector.is_stream());
        assert_eq!(inspector.mode, InspectorMode::Pretty);
    }

    #[test]
    fn cursor_moves_over_refs_and_reports_current() {
        let mut inspector = InspectorState::new();
        let mut dict = Dict::new();
        dict.insert(Name("A".to_string()), Object::Ref(ObjRef { num: 7, gen: 0 }));
        dict.insert(Name("B".to_string()), Object::Ref(ObjRef { num: 9, gen: 0 }));
        inspector.set_object(ObjRef { num: 1, gen: 0 }, Object::Dict(dict));
        assert_eq!(inspector.current_ref(), None);
        inspector.move_cursor(1);
        assert_eq!(inspector.current_ref(), Some(ObjRef { num: 7, gen: 0 }));
        inspector.move_cursor(1);
        assert_eq!(inspector.current_ref(), Some(ObjRef { num: 9, gen: 0 }));
        inspector.move_cursor(1);
        assert_eq!(
            inspector.current_ref(),
            Some(ObjRef { num: 9, gen: 0 }),
            "clamped at last ref"
        );
        inspector.move_cursor(-5);
        assert_eq!(inspector.current_ref(), Some(ObjRef { num: 7, gen: 0 }));
    }

    #[test]
    fn cycle_mode_on_non_stream_stays_pretty() {
        let mut inspector = InspectorState::new();
        inspector.set_object(ObjRef { num: 1, gen: 0 }, catalog());
        assert!(!inspector.cycle_mode());
        assert_eq!(inspector.mode, InspectorMode::Pretty);
    }

    #[test]
    fn cycle_mode_walks_stream_views_and_requests_decode() {
        let mut inspector = InspectorState::new();
        inspector.set_object(ObjRef { num: 4, gen: 0 }, content_stream());
        assert!(!inspector.cycle_mode(), "raw needs no decode");
        assert_eq!(inspector.mode, InspectorMode::Raw);
        assert_eq!(inspector.lines, vec!["raw-bytes"]);
        assert!(inspector.cycle_mode(), "decoded view needs data");
        assert_eq!(inspector.mode, InspectorMode::Decoded);
        assert_eq!(inspector.lines, vec!["decoding\u{2026}"]);
        inspector.set_decoded(ObjRef { num: 4, gen: 0 }, b"BT /F1 12 Tf ET".to_vec());
        assert_eq!(inspector.lines, vec!["BT /F1 12 Tf ET"]);
        assert!(!inspector.cycle_mode(), "ops reuses decoded data");
        assert_eq!(inspector.mode, InspectorMode::Ops);
        assert_eq!(
            inspector.lines,
            vec!["BeginText", "SetFont(Name(\"F1\"), 12.0)", "EndText"]
        );
        assert!(!inspector.cycle_mode());
        assert_eq!(inspector.mode, InspectorMode::Pretty);
    }

    #[test]
    fn stale_decoded_payload_is_ignored() {
        let mut inspector = InspectorState::new();
        inspector.set_object(ObjRef { num: 4, gen: 0 }, content_stream());
        inspector.cycle_mode();
        inspector.cycle_mode();
        inspector.set_decoded(ObjRef { num: 9, gen: 0 }, b"junk".to_vec());
        assert_eq!(inspector.lines, vec!["decoding\u{2026}"], "wrong object dropped");
    }

    #[test]
    fn ops_lines_reports_parse_failure_inline() {
        let lines = ops_lines(&[0x28, 0x61]); // unterminated string literal
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("content parse failed: "));
    }

    #[test]
    fn bytes_lines_replaces_non_printable_and_caps_output() {
        assert_eq!(bytes_lines(b"ab\ncd"), vec!["ab", "cd"]);
        assert_eq!(bytes_lines(&[0x00, 0x41]), vec!["\u{b7}A"]);
        let big = vec![b'\n'; 2500];
        let lines = bytes_lines(&big);
        assert_eq!(lines.len(), 2001);
        assert_eq!(lines[2000], "\u{2026} (truncated)");
    }

    #[test]
    fn show_message_and_loading() {
        let mut inspector = InspectorState::new();
        inspector.show_message("Document", vec!["version: 1.7".to_string()]);
        assert_eq!(inspector.title, "Document");
        assert_eq!(inspector.lines, vec!["version: 1.7"]);
        assert!(inspector.refs.is_empty());
        inspector.show_loading("obj 3 0");
        assert!(inspector.loading);
        assert_eq!(inspector.lines, vec!["loading\u{2026}"]);
    }
}
```

- [ ] Add `pub mod inspector;` to `crates/pdfboss-tui/src/lib.rs`.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui inspector` — expect compile errors.
- [ ] Implement. Prepend to `crates/pdfboss-tui/src/inspector.rs`:

```rust
//! Inspector pane: the selected element pretty-printed, with `d` cycling
//! raw bytes / decoded bytes / disassembled content operators for streams,
//! and a cursor over `N G R` references for Enter-to-jump.

use pdfboss_core::content::parse_content;
use pdfboss_core::pretty;
use pdfboss_core::{Object, ObjRef};

/// Maximum lines the Raw/Decoded byte views materialize.
const MAX_BYTE_LINES: usize = 2000;

/// Which view of the selection is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InspectorMode {
    Pretty,
    Raw,
    Decoded,
    Ops,
}

/// Async payloads the inspector consumes.
#[derive(Debug, Clone)]
pub enum InspectorPayload {
    /// The parsed object (streams carry raw data in `Object::Stream`).
    Object { r: ObjRef, object: Object },
    /// Decoded stream data for the Decoded/Ops views.
    Decoded { r: ObjRef, data: Vec<u8> },
}

/// Inspector pane model.
pub struct InspectorState {
    pub title: String,
    pub object: Option<(ObjRef, Object)>,
    pub decoded: Option<Vec<u8>>,
    pub mode: InspectorMode,
    pub scroll: u16,
    pub lines: Vec<String>,
    /// `(line index, ref)` pairs found in the Pretty text, display order.
    pub refs: Vec<(usize, ObjRef)>,
    pub ref_cursor: Option<usize>,
    pub loading: bool,
}

impl InspectorState {
    pub fn new() -> InspectorState {
        InspectorState {
            title: String::new(),
            object: None,
            decoded: None,
            mode: InspectorMode::Pretty,
            scroll: 0,
            lines: Vec::new(),
            refs: Vec::new(),
            ref_cursor: None,
            loading: false,
        }
    }

    /// Shows plain informational lines (folders, xref summaries, errors).
    pub fn show_message(&mut self, title: &str, lines: Vec<String>) {
        self.title = title.to_string();
        self.object = None;
        self.decoded = None;
        self.mode = InspectorMode::Pretty;
        self.scroll = 0;
        self.lines = lines;
        self.refs = Vec::new();
        self.ref_cursor = None;
        self.loading = false;
    }

    /// Placeholder while an object fetch is in flight.
    pub fn show_loading(&mut self, title: &str) {
        self.show_message(title, vec!["loading\u{2026}".to_string()]);
        self.loading = true;
    }

    /// Installs a fetched object and rebuilds the Pretty view.
    pub fn set_object(&mut self, r: ObjRef, object: Object) {
        self.title = format!("obj {} {}", r.num, r.gen);
        self.object = Some((r, object));
        self.decoded = None;
        self.mode = InspectorMode::Pretty;
        self.scroll = 0;
        self.ref_cursor = None;
        self.loading = false;
        self.rebuild();
    }

    /// Installs decoded stream data (ignored unless it matches the shown
    /// object) and refreshes decoded-backed views.
    pub fn set_decoded(&mut self, r: ObjRef, data: Vec<u8>) {
        let matches_current = self
            .object
            .as_ref()
            .is_some_and(|(shown, ..)| shown.num == r.num && shown.gen == r.gen);
        if !matches_current {
            return;
        }
        self.decoded = Some(data);
        if matches!(self.mode, InspectorMode::Decoded | InspectorMode::Ops) {
            self.rebuild();
        }
    }

    /// Whether the shown object is a stream (enables `d` cycling).
    pub fn is_stream(&self) -> bool {
        self.object
            .as_ref()
            .is_some_and(|(.., object)| object.as_stream().is_some())
    }

    /// Cycles Pretty → Raw → Decoded → Ops → Pretty on streams. Returns
    /// true when the new view needs decoded data not yet present.
    pub fn cycle_mode(&mut self) -> bool {
        if !self.is_stream() {
            return false;
        }
        self.mode = match self.mode {
            InspectorMode::Pretty => InspectorMode::Raw,
            InspectorMode::Raw => InspectorMode::Decoded,
            InspectorMode::Decoded => InspectorMode::Ops,
            InspectorMode::Ops => InspectorMode::Pretty,
        };
        self.scroll = 0;
        self.rebuild();
        matches!(self.mode, InspectorMode::Decoded | InspectorMode::Ops)
            && self.decoded.is_none()
    }

    /// Short mode name for the pane title.
    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            InspectorMode::Pretty => "pretty",
            InspectorMode::Raw => "raw",
            InspectorMode::Decoded => "decoded",
            InspectorMode::Ops => "ops",
        }
    }

    /// Moves the ref cursor (Pretty view); scroll follows the cursor line.
    pub fn move_cursor(&mut self, delta: i32) {
        if self.refs.is_empty() {
            let next = i64::from(self.scroll) + i64::from(delta);
            self.scroll = next.clamp(0, self.lines.len().saturating_sub(1) as i64) as u16;
            return;
        }
        let last = self.refs.len() - 1;
        let next = match self.ref_cursor {
            None if delta > 0 => 0,
            None => return,
            Some(index) => {
                let moved = index as i64 + i64::from(delta);
                moved.clamp(0, last as i64) as usize
            }
        };
        self.ref_cursor = Some(next);
        let line = self.refs[next].0;
        self.scroll = line.saturating_sub(2) as u16;
    }

    /// The ref under the cursor, for Enter-to-jump.
    pub fn current_ref(&self) -> Option<ObjRef> {
        let index = self.ref_cursor?;
        Some(self.refs.get(index)?.1)
    }

    fn rebuild(&mut self) {
        let Some((.., object)) = self.object.as_ref() else {
            return;
        };
        match self.mode {
            InspectorMode::Pretty => {
                let text = pretty::format_object(object);
                self.lines = text.lines().map(str::to_string).collect();
                self.refs = ref_lines(&text);
            }
            InspectorMode::Raw => {
                let raw: &[u8] = match object.as_stream() {
                    Some(stream) => &stream.data,
                    None => &[],
                };
                self.lines = bytes_lines(raw);
                self.refs = Vec::new();
                self.ref_cursor = None;
            }
            InspectorMode::Decoded => {
                self.lines = match self.decoded.as_deref() {
                    Some(data) => bytes_lines(data),
                    None => vec!["decoding\u{2026}".to_string()],
                };
                self.refs = Vec::new();
                self.ref_cursor = None;
            }
            InspectorMode::Ops => {
                self.lines = match self.decoded.as_deref() {
                    Some(data) => ops_lines(data),
                    None => vec!["decoding\u{2026}".to_string()],
                };
                self.refs = Vec::new();
                self.ref_cursor = None;
            }
        }
    }
}

impl Default for InspectorState {
    fn default() -> InspectorState {
        InspectorState::new()
    }
}

/// Scans pretty-printed text for `N G R` reference tokens, returning
/// `(line index, ref)` pairs in display order.
pub fn ref_lines(text: &str) -> Vec<(usize, ObjRef)> {
    let mut found = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let tokens: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
            .filter(|token| !token.is_empty())
            .collect();
        for window in tokens.windows(3) {
            if window[2] != "R" {
                continue;
            }
            let (Ok(num), Ok(gen)) = (window[0].parse::<u32>(), window[1].parse::<u16>())
            else {
                continue;
            };
            found.push((line_index, ObjRef { num, gen }));
        }
    }
    found
}

/// Byte views: split on newlines, map non-printable bytes to `·`, cap at
/// [`MAX_BYTE_LINES`] lines.
pub fn bytes_lines(data: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for segment in data.split(|byte| *byte == b'\n') {
        if lines.len() == MAX_BYTE_LINES {
            lines.push("\u{2026} (truncated)".to_string());
            return lines;
        }
        let text: String = segment
            .iter()
            .map(|byte| {
                if (0x20..=0x7e).contains(byte) {
                    char::from(*byte)
                } else {
                    '\u{b7}'
                }
            })
            .collect();
        lines.push(text);
    }
    lines
}

/// Disassembles decoded content-stream bytes, one operator per line.
pub fn ops_lines(data: &[u8]) -> Vec<String> {
    match parse_content(data) {
        Ok(ops) => ops.iter().map(|op| format!("{:?}", op)).collect(),
        Err(error) => vec![format!("content parse failed: {error}")],
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui inspector` — expect all 9 tests green. If `bytes_lines(&[b'\n'; 2500])` yields a different count, recheck the cap logic: 2500 newlines produce 2501 segments; the 2001st entry must be the truncation marker (lines.len() == 2001).
- [ ] Run clippy + fmt — expect clean.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): inspector with pretty/raw/decoded/ops views and ref cursor"`

---

### Task 6: Preview model (`preview.rs`)

**Files:**
- Create: `crates/pdfboss-tui/src/preview.rs`
- Modify: `crates/pdfboss-tui/src/lib.rs` (add module declaration)
- Test: unit tests inside `crates/pdfboss-tui/src/preview.rs`

**Interfaces:**
- Consumes: `pdfboss_render::Pixmap { pub width: u32, pub height: u32, pub data: Vec<u8> }` (RGBA8, straight alpha, row-major from top-left — `crates/pdfboss-render/src/lib.rs:54`); ratatui `Color`.
- Produces: `SPINNER: [char; 4]`, `RESIZE_DEBOUNCE_TICKS: u8 = 2` (ticks are 100 ms → ~200 ms debounce), `PreviewFrame { file_bytes: Arc<Vec<u8>>, pixmap: Pixmap }`, `PreviewState` (+ `new/start_render/apply_ready/tick`), `fit_scale(page_w: f32, page_h: f32, max_w: u32, max_h: u32) -> f32`, `cell_colors(pix: &Pixmap, x: u32, row: u32) -> (Color, Color)`.

**Steps:**

- [ ] Write the failing tests. Create `crates/pdfboss-tui/src/preview.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn two_by_two() -> Pixmap {
        // Row 0: red, green; row 1: blue, transparent.
        Pixmap {
            width: 2,
            height: 2,
            data: vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 0, 0, 0, 0,
            ],
        }
    }

    #[test]
    fn fit_scale_fits_both_axes() {
        assert_eq!(fit_scale(100.0, 100.0, 200, 50), 0.5);
        assert_eq!(fit_scale(100.0, 100.0, 50, 200), 0.5);
        assert_eq!(fit_scale(612.0, 792.0, 612, 792), 1.0);
        assert_eq!(fit_scale(0.0, 100.0, 50, 50), 1.0, "degenerate page");
        assert!(fit_scale(1_000_000.0, 1.0, 10, 10) >= 0.001, "clamped floor");
    }

    #[test]
    fn cell_colors_pack_two_rows_per_cell() {
        let pix = two_by_two();
        assert_eq!(
            cell_colors(&pix, 0, 0),
            (Color::Rgb(255, 0, 0), Color::Rgb(0, 0, 255))
        );
        // Transparent blends to white; out-of-range pixels are white.
        assert_eq!(
            cell_colors(&pix, 1, 0),
            (Color::Rgb(0, 255, 0), Color::Rgb(255, 255, 255))
        );
        assert_eq!(
            cell_colors(&pix, 5, 9),
            (Color::Rgb(255, 255, 255), Color::Rgb(255, 255, 255))
        );
    }

    #[test]
    fn start_render_bumps_generation_and_spins() {
        let mut preview = PreviewState::new();
        let first = preview.start_render(0);
        let second = preview.start_render(0);
        assert!(second > first);
        assert!(preview.rendering);
        let before = preview.spinner_frame;
        assert!(!preview.tick());
        assert_ne!(preview.spinner_frame, before, "spinner advances while rendering");
    }

    #[test]
    fn apply_ready_ignores_stale_generations() {
        let mut preview = PreviewState::new();
        let stale = preview.start_render(0);
        let current = preview.start_render(0);
        let frame = PreviewFrame {
            file_bytes: Arc::new(vec![1, 2, 3]),
            pixmap: two_by_two(),
        };
        assert!(!preview.apply_ready(stale, Ok(frame)));
        assert!(preview.rendering, "stale result leaves the spinner on");
        let frame = PreviewFrame {
            file_bytes: Arc::new(vec![1, 2, 3]),
            pixmap: two_by_two(),
        };
        assert!(preview.apply_ready(current, Ok(frame)));
        assert!(!preview.rendering);
        assert!(preview.pixmap.is_some());
        assert!(preview.file_bytes.is_some(), "bytes cached for re-renders");
        assert!(preview.apply_ready(current, Err("boom".to_string())));
        assert_eq!(preview.error.as_deref(), Some("boom"));
    }

    #[test]
    fn debounce_counts_down_to_render_request() {
        let mut preview = PreviewState::new();
        preview.active = true;
        preview.debounce = Some(RESIZE_DEBOUNCE_TICKS);
        assert!(!preview.tick());
        assert!(preview.tick(), "second tick fires the deferred render");
        assert_eq!(preview.debounce, None);
        assert!(!preview.tick(), "no further fires");
    }
}
```

- [ ] Add `pub mod preview;` to `crates/pdfboss-tui/src/lib.rs`.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui preview` — expect compile errors.
- [ ] Implement. Prepend to `crates/pdfboss-tui/src/preview.rs`:

```rust
//! Page preview: a rasterized page painted with `▀` half-blocks — the
//! upper pixel of each terminal cell is the foreground color, the lower
//! pixel the background color, two vertical pixels per cell. Rendering
//! happens off the event loop; this module is pure state and math.

use std::sync::Arc;

use pdfboss_render::Pixmap;
use ratatui::style::Color;

/// Spinner frames shown while a render is in flight.
pub const SPINNER: [char; 4] = ['|', '/', '-', '\\'];
/// Resize debounce in 100 ms ticks (~200 ms).
pub const RESIZE_DEBOUNCE_TICKS: u8 = 2;

/// A finished render plus the file bytes fetched for it (cached so later
/// renders skip the fetch).
#[derive(Debug)]
pub struct PreviewFrame {
    pub file_bytes: Arc<Vec<u8>>,
    pub pixmap: Pixmap,
}

/// Preview pane model.
pub struct PreviewState {
    /// Whether the preview replaces the inspector (`p`).
    pub active: bool,
    pub page: Option<usize>,
    pub pixmap: Option<Pixmap>,
    pub rendering: bool,
    pub spinner_frame: usize,
    pub generation: u64,
    pub file_bytes: Option<Arc<Vec<u8>>>,
    /// Ticks until a resize-deferred re-render fires.
    pub debounce: Option<u8>,
    pub error: Option<String>,
}

impl PreviewState {
    pub fn new() -> PreviewState {
        PreviewState {
            active: false,
            page: None,
            pixmap: None,
            rendering: false,
            spinner_frame: 0,
            generation: 0,
            file_bytes: None,
            debounce: None,
            error: None,
        }
    }

    /// Marks a render in flight for `page`; returns its generation.
    pub fn start_render(&mut self, page: usize) -> u64 {
        self.generation += 1;
        self.page = Some(page);
        self.rendering = true;
        self.error = None;
        self.debounce = None;
        self.generation
    }

    /// Applies a finished render; stale generations are dropped. Returns
    /// whether the result was accepted.
    pub fn apply_ready(
        &mut self,
        generation: u64,
        result: Result<PreviewFrame, String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.rendering = false;
        match result {
            Ok(frame) => {
                self.file_bytes = Some(frame.file_bytes);
                self.pixmap = Some(frame.pixmap);
                self.error = None;
            }
            Err(message) => self.error = Some(message),
        }
        true
    }

    /// 100 ms heartbeat: advances the spinner and counts the resize
    /// debounce down. Returns true when a deferred re-render should fire.
    pub fn tick(&mut self) -> bool {
        if self.rendering {
            self.spinner_frame = (self.spinner_frame + 1) % SPINNER.len();
        }
        match self.debounce {
            Some(0) | None => {
                self.debounce = None;
                false
            }
            Some(1) => {
                self.debounce = None;
                self.active
            }
            Some(remaining) => {
                self.debounce = Some(remaining - 1);
                false
            }
        }
    }
}

impl Default for PreviewState {
    fn default() -> PreviewState {
        PreviewState::new()
    }
}

/// The scale that fits a `page_w x page_h` point page inside a
/// `max_w x max_h` pixel budget, preserving aspect ratio.
pub fn fit_scale(page_w: f32, page_h: f32, max_w: u32, max_h: u32) -> f32 {
    if !(page_w.is_finite() && page_h.is_finite()) || page_w <= 0.0 || page_h <= 0.0 {
        return 1.0;
    }
    let horizontal = max_w as f32 / page_w;
    let vertical = max_h as f32 / page_h;
    horizontal.min(vertical).max(0.001)
}

/// RGBA (straight alpha) composited over the white page background.
fn blend_over_white(rgba: [u8; 4]) -> Color {
    let alpha = rgba[3] as u32;
    let channel = |value: u8| -> u8 {
        ((value as u32 * alpha + 255 * (255 - alpha)) / 255) as u8
    };
    Color::Rgb(channel(rgba[0]), channel(rgba[1]), channel(rgba[2]))
}

fn pixel(pix: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    if x >= pix.width || y >= pix.height {
        return [255, 255, 255, 255];
    }
    let index = ((y * pix.width + x) * 4) as usize;
    [
        pix.data[index],
        pix.data[index + 1],
        pix.data[index + 2],
        pix.data[index + 3],
    ]
}

/// The `(foreground, background)` of terminal cell `(x, row)`: pixel rows
/// `2*row` (upper, fg of `▀`) and `2*row + 1` (lower, bg).
pub fn cell_colors(pix: &Pixmap, x: u32, row: u32) -> (Color, Color) {
    (
        blend_over_white(pixel(pix, x, row * 2)),
        blend_over_white(pixel(pix, x, row * 2 + 1)),
    )
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui preview` — expect all 5 tests green.
- [ ] Run clippy + fmt — expect clean.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): half-block page preview model with debounce and spinner"`

---

### Task 7: Key mapping (`input.rs`)

**Files:**
- Create: `crates/pdfboss-tui/src/input.rs`
- Modify: `crates/pdfboss-tui/src/lib.rs` (add module declaration)
- Test: unit tests inside `crates/pdfboss-tui/src/input.rs`

**Interfaces:**
- Consumes: `crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers}`.
- Produces: `Action` enum and `action_for(key: KeyEvent, search_input: bool) -> Action`.

**Steps:**

- [ ] Write the failing tests. Create `crates/pdfboss-tui/src/input.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn normal_mode_bindings() {
        assert_eq!(action_for(press(KeyCode::Char('q')), false), Action::Quit);
        assert_eq!(action_for(press(KeyCode::Esc), false), Action::Quit);
        assert_eq!(action_for(press(KeyCode::Char('/')), false), Action::OpenSearch);
        assert_eq!(action_for(press(KeyCode::Char('j')), false), Action::MoveDown);
        assert_eq!(action_for(press(KeyCode::Down), false), Action::MoveDown);
        assert_eq!(action_for(press(KeyCode::Char('k')), false), Action::MoveUp);
        assert_eq!(action_for(press(KeyCode::Up), false), Action::MoveUp);
        assert_eq!(action_for(press(KeyCode::Char('h')), false), Action::Collapse);
        assert_eq!(action_for(press(KeyCode::Left), false), Action::Collapse);
        assert_eq!(action_for(press(KeyCode::Char('l')), false), Action::Expand);
        assert_eq!(action_for(press(KeyCode::Right), false), Action::Expand);
        assert_eq!(action_for(press(KeyCode::Tab), false), Action::FocusNext);
        assert_eq!(action_for(press(KeyCode::Enter), false), Action::Activate);
        assert_eq!(action_for(press(KeyCode::Backspace), false), Action::Back);
        assert_eq!(action_for(press(KeyCode::Char('g')), false), Action::Top);
        assert_eq!(action_for(press(KeyCode::Char('G')), false), Action::Bottom);
        assert_eq!(action_for(press(KeyCode::PageUp), false), Action::PageUp);
        assert_eq!(action_for(press(KeyCode::PageDown), false), Action::PageDown);
        assert_eq!(action_for(press(KeyCode::Char('d')), false), Action::CycleView);
        assert_eq!(action_for(press(KeyCode::Char('p')), false), Action::TogglePreview);
        assert_eq!(action_for(press(KeyCode::Char('n')), false), Action::NextHit);
        assert_eq!(action_for(press(KeyCode::Char('N')), false), Action::PrevHit);
        assert_eq!(action_for(press(KeyCode::Char('z')), false), Action::Noop);
    }

    #[test]
    fn search_mode_routes_text_input() {
        assert_eq!(
            action_for(press(KeyCode::Char('q')), true),
            Action::SearchChar('q'),
            "q types into the query instead of quitting"
        );
        assert_eq!(action_for(press(KeyCode::Esc), true), Action::SearchCancel);
        assert_eq!(action_for(press(KeyCode::Enter), true), Action::SearchAccept);
        assert_eq!(
            action_for(press(KeyCode::Backspace), true),
            Action::SearchBackspace
        );
        assert_eq!(action_for(press(KeyCode::Tab), true), Action::Noop);
    }

    #[test]
    fn key_release_is_ignored() {
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(action_for(release, false), Action::Noop);
    }
}
```

- [ ] Add `pub mod input;` to `crates/pdfboss-tui/src/lib.rs`.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui input` — expect compile errors.
- [ ] Implement. Prepend to `crates/pdfboss-tui/src/input.rs`:

```rust
//! Key-event → intent mapping. Pure so bindings are unit-testable.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

/// Every intent a key press can express.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    MoveUp,
    MoveDown,
    Collapse,
    Expand,
    FocusNext,
    Activate,
    Back,
    Top,
    Bottom,
    PageUp,
    PageDown,
    CycleView,
    TogglePreview,
    OpenSearch,
    SearchChar(char),
    SearchBackspace,
    SearchAccept,
    SearchCancel,
    NextHit,
    PrevHit,
    Quit,
    Noop,
}

/// Maps a key event. `search_input` reroutes printable keys into the
/// status-bar query (Esc cancels the search instead of quitting).
pub fn action_for(key: KeyEvent, search_input: bool) -> Action {
    if key.kind != KeyEventKind::Press {
        return Action::Noop;
    }
    if search_input {
        return match key.code {
            KeyCode::Esc => Action::SearchCancel,
            KeyCode::Enter => Action::SearchAccept,
            KeyCode::Backspace => Action::SearchBackspace,
            KeyCode::Char(c) => Action::SearchChar(c),
            _ => Action::Noop,
        };
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Char('/') => Action::OpenSearch,
        KeyCode::Char('n') => Action::NextHit,
        KeyCode::Char('N') => Action::PrevHit,
        KeyCode::Up | KeyCode::Char('k') => Action::MoveUp,
        KeyCode::Down | KeyCode::Char('j') => Action::MoveDown,
        KeyCode::Left | KeyCode::Char('h') => Action::Collapse,
        KeyCode::Right | KeyCode::Char('l') => Action::Expand,
        KeyCode::Tab => Action::FocusNext,
        KeyCode::Enter => Action::Activate,
        KeyCode::Backspace => Action::Back,
        KeyCode::Char('g') => Action::Top,
        KeyCode::Char('G') => Action::Bottom,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Char('d') => Action::CycleView,
        KeyCode::Char('p') => Action::TogglePreview,
        _ => Action::Noop,
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui input` — expect all 3 tests green.
- [ ] Run clippy + fmt — expect clean.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): key-to-action mapping"`

---

### Task 8: Layout math and the App state machine (`ui.rs` panes + `app.rs`)

**Files:**
- Create: `crates/pdfboss-tui/src/ui.rs` (layout math only in this task; rendering lands in Task 9)
- Create: `crates/pdfboss-tui/src/app.rs`
- Modify: `crates/pdfboss-tui/src/lib.rs` (add both module declarations)
- Modify: `crates/pdfboss-tui/src/inspector.rs` (add `set_dict`; full method shown below)
- Test: unit tests inside `crates/pdfboss-tui/src/ui.rs` and `crates/pdfboss-tui/src/app.rs`

**Interfaces:**
- Consumes: everything produced by Tasks 2–7; `pdfboss_core::pretty::format_dict` (plan 01); ratatui `Layout/Constraint/Rect`.
- Produces:
  - `ui::Panes { tree: Rect, right_top: Rect, hex: Rect, status: Rect }`, `ui::panes(area: Rect) -> Panes` (tree 35% wide; right column split 60/40 inspector-or-preview over hex; 1-row status bar).
  - `app::Pane { Tree, Inspector, Hex }`, `app::Msg` (shown fully below), `app::Cmd` (shown fully below), `app::App` with `new(title, version, page_count, size) -> App`, `update(&mut self, msg: Msg) -> Vec<Cmd>`, `status_line(&self) -> String`, and public fields `title, tree, inspector, hex, preview, search, focus, history, pending_jump, toast, size, should_quit, inspector_generation, hex_generation`.
  - `inspector::InspectorState::set_dict(&mut self, title: &str, dict: &Dict)`.

**Steps:**

- [ ] Add `set_dict` to `crates/pdfboss-tui/src/inspector.rs` (inside `impl InspectorState`, after `set_decoded`) plus `use pdfboss_core::Dict;` in that file's imports — the trailer is a bare dictionary whose refs must be jumpable:

```rust
    /// Shows a bare dictionary (the trailer) pretty-printed, refs jumpable.
    pub fn set_dict(&mut self, title: &str, dict: &Dict) {
        let text = pretty::format_dict(dict);
        self.title = title.to_string();
        self.object = None;
        self.decoded = None;
        self.mode = InspectorMode::Pretty;
        self.scroll = 0;
        self.lines = text.lines().map(str::to_string).collect();
        self.refs = ref_lines(&text);
        self.ref_cursor = None;
        self.loading = false;
    }
```

- [ ] Create `crates/pdfboss-tui/src/ui.rs` with the layout math and its test (rendering functions come in Task 9):

```rust
//! Rendering: pure layout math here plus (Task 9) the pane painters.

use ratatui::layout::{Constraint, Layout, Rect};

/// The four screen regions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Panes {
    pub tree: Rect,
    /// Inspector, or the page preview while it is active.
    pub right_top: Rect,
    pub hex: Rect,
    pub status: Rect,
}

/// Splits the terminal: status bar (1 row) at the bottom; tree pane at
/// ~35% width; right column split 60/40 into inspector and hex.
pub fn panes(area: Rect) -> Panes {
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let columns =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(rows[0]);
    let right =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(columns[1]);
    Panes {
        tree: columns[0],
        right_top: right[0],
        hex: right[1],
        status: rows[1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panes_split_80_by_24_deterministically() {
        let split = panes(Rect::new(0, 0, 80, 24));
        assert_eq!(split.tree, Rect::new(0, 0, 28, 23));
        assert_eq!(split.right_top, Rect::new(28, 0, 52, 14));
        assert_eq!(split.hex, Rect::new(28, 14, 52, 9));
        assert_eq!(split.status, Rect::new(0, 23, 80, 1));
    }
}
```

- [ ] Add `pub mod ui;` and `pub mod app;` to `crates/pdfboss-tui/src/lib.rs`.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui ui::` — if the Rect assertions fail, the cassowary solver rounded differently: fix the expected Rects in this test to the actual solver output **and** propagate the same numbers to the Task 9 snapshot frames (they assume 28/52 columns and 14/9 rows).
- [ ] Write the failing app tests. Create `crates/pdfboss-tui/src/app.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use pdfboss_core::elements::{Element, Span, XrefKind};
    use pdfboss_core::{Dict, Name, Object, ObjRef, Stream};

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn obj_ref(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    fn physical_elements() -> Vec<Element> {
        let mut trailer = Dict::new();
        trailer.insert(
            Name("Root".to_string()),
            Object::Ref(obj_ref(1)),
        );
        vec![
            Element::Header {
                version: (1, 7),
                span: Span { start: 0, end: 15 },
            },
            Element::IndirectObject {
                r: obj_ref(1),
                object: Object::Null,
                span: Span { start: 15, end: 64 },
                in_objstm: None,
            },
            Element::IndirectObject {
                r: obj_ref(2),
                object: Object::Null,
                span: Span { start: 64, end: 120 },
                in_objstm: None,
            },
            Element::XrefSection {
                kind: XrefKind::Table,
                span: Span { start: 120, end: 260 },
                entries: 3,
            },
            Element::Trailer {
                dict: trailer,
                span: Span { start: 260, end: 300 },
            },
        ]
    }

    fn loaded_app() -> App {
        let mut app = App::new("t.pdf".to_string(), (1, 7), 1, (80, 24));
        let cmds = app.update(Msg::TreeBatch {
            req: crate::tree::TreeReq::Physical,
            elements: physical_elements(),
            errors: 0,
            done: true,
        });
        assert!(cmds.is_empty(), "root selection refresh needs no data");
        app
    }

    #[test]
    fn quit_sets_flag() {
        let mut app = loaded_app();
        app.update(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn selecting_an_object_loads_inspector_and_hex() {
        let mut app = loaded_app();
        app.update(key(KeyCode::Char('j'))); // Pages
        app.update(key(KeyCode::Char('j'))); // Objects
        app.update(key(KeyCode::Char('l'))); // expand (already loaded)
        let cmds = app.update(key(KeyCode::Char('j'))); // obj 1 0
        assert!(cmds.iter().any(|cmd| matches!(
            cmd,
            Cmd::LoadObject { r, .. } if r.num == 1
        )));
        assert!(cmds.iter().any(|cmd| matches!(
            cmd,
            Cmd::LoadHex { source: crate::hexview::HexSource::File { span }, window_start: 0, .. }
                if span.start == 15 && span.end == 64
        )));
        assert_eq!(app.inspector.title, "obj 1 0");
        assert!(app.inspector.loading);
    }

    #[test]
    fn search_hit_jump_and_backspace_history() {
        let mut app = loaded_app();
        let root = app.tree.selected;
        app.update(key(KeyCode::Char('/')));
        assert!(app.search.active);
        let cmds = app.update(key(KeyCode::Char('2')));
        let generation = match cmds.as_slice() {
            [Cmd::StartSearch { generation, query }] => {
                assert_eq!(query, "2");
                *generation
            }
            other => panic!("expected StartSearch, got {:?}", other),
        };
        app.update(Msg::SearchResult {
            generation,
            hit: crate::search::SearchHit { r: obj_ref(2) },
        });
        app.update(Msg::SearchDone { generation });
        app.update(key(KeyCode::Enter)); // accept, keep hits
        assert!(!app.search.active);
        let cmds = app.update(key(KeyCode::Char('n')));
        assert!(cmds.iter().any(|cmd| matches!(
            cmd,
            Cmd::LoadObject { r, .. } if r.num == 2
        )));
        let jumped = app.tree.selected;
        assert_ne!(jumped, root);
        assert_eq!(app.history, vec![root]);
        // Backspace pops the jump history.
        let cmds = app.update(key(KeyCode::Backspace));
        assert_eq!(app.tree.selected, root);
        assert!(app.history.is_empty());
        assert!(cmds.is_empty(), "root selection needs no fetches");
    }

    #[test]
    fn jump_before_physical_load_defers() {
        let mut app = App::new("t.pdf".to_string(), (1, 7), 1, (80, 24));
        let cmds = app.jump_to(obj_ref(2));
        assert!(matches!(
            cmds.as_slice(),
            [Cmd::LoadTree(crate::tree::TreeReq::Physical)]
        ));
        assert_eq!(app.pending_jump, Some(obj_ref(2)));
        let cmds = app.update(Msg::TreeBatch {
            req: crate::tree::TreeReq::Physical,
            elements: physical_elements(),
            errors: 0,
            done: true,
        });
        assert_eq!(app.pending_jump, None);
        assert!(cmds.iter().any(|cmd| matches!(
            cmd,
            Cmd::LoadObject { r, .. } if r.num == 2
        )));
    }

    #[test]
    fn d_cycles_stream_views_and_requests_decode() {
        let mut app = loaded_app();
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('l')));
        app.update(key(KeyCode::Char('j'))); // obj 1 0
        app.update(Msg::InspectorLoaded {
            generation: app.inspector_generation,
            payload: crate::inspector::InspectorPayload::Object {
                r: obj_ref(1),
                object: Object::Stream(Stream {
                    dict: Dict::new(),
                    data: b"BT ET".to_vec(),
                }),
            },
        });
        let cmds = app.update(key(KeyCode::Char('d'))); // raw: no fetch
        assert!(cmds.is_empty());
        let cmds = app.update(key(KeyCode::Char('d'))); // decoded: fetch
        assert!(matches!(
            cmds.as_slice(),
            [Cmd::DecodeStream { r, .. }] if r.num == 1
        ));
    }

    #[test]
    fn resize_debounces_preview_rerender() {
        let mut app = loaded_app();
        let cmds = app.update(key(KeyCode::Char('p')));
        assert!(app.preview.active);
        assert!(matches!(
            cmds.as_slice(),
            [Cmd::RenderPreview { page: 0, .. }]
        ));
        let cmds = app.update(Msg::Resize(100, 40));
        assert!(cmds.is_empty(), "resize alone renders nothing");
        assert!(app.update(Msg::Tick).is_empty(), "first tick still waiting");
        let cmds = app.update(Msg::Tick);
        assert!(
            matches!(cmds.as_slice(), [Cmd::RenderPreview { .. }]),
            "debounced re-render after ~200 ms"
        );
    }

    #[test]
    fn toast_expires_after_ticks() {
        let mut app = loaded_app();
        app.toast("hello");
        assert_eq!(app.status_line(), "hello");
        for count in 0..30 {
            let ignored_len = app.update(Msg::Tick).len();
            assert_eq!(ignored_len, 0, "tick {count} spawned nothing");
        }
        assert!(app.status_line().starts_with("t.pdf \u{b7} /Document"));
    }

    #[test]
    fn status_line_shows_search_then_breadcrumb() {
        let mut app = loaded_app();
        assert_eq!(
            app.status_line(),
            "t.pdf \u{b7} /Document \u{b7} [/] search  [p] preview  [q] quit"
        );
        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.status_line(), "/a \u{b7} 0 hits \u{2026}");
    }

    #[test]
    fn hex_scrolling_requests_missing_windows() {
        let mut app = loaded_app();
        // Select the trailer (span 260..300) whose hex loads on selection.
        app.update(key(KeyCode::Char('G')));
        assert_eq!(app.tree.selected, app.tree.trailer_node);
        app.update(Msg::HexLoaded {
            generation: app.hex_generation,
            window_start: 0,
            total_len: 40,
            bytes: vec![0u8; 40],
        });
        // Focus the hex pane (Tree → Inspector → Hex) and scroll.
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Hex);
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.hex.scroll_line, 1);
        app.update(key(KeyCode::PageDown));
        assert_eq!(app.hex.scroll_line, 4, "clamped to the 5-line span");
        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.hex.scroll_line, 0);
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui app` — expect compile errors (missing `App`, `Msg`, `Cmd`).
- [ ] Implement. Prepend to `crates/pdfboss-tui/src/app.rs`:

```rust
//! The application state machine. `update` consumes [`Msg`]s (input,
//! ticks, background-task completions) and returns [`Cmd`]s (side effects
//! for the event loop to execute); no I/O happens here, which keeps the
//! whole TUI testable without a terminal.

use std::sync::Arc;

use crossterm::event::KeyEvent;
use pdfboss_core::elements::{Element, Span};
use pdfboss_core::ObjRef;

use crate::hexview::{HexSource, HexState};
use crate::input::{action_for, Action};
use crate::inspector::{InspectorPayload, InspectorState};
use crate::preview::{PreviewFrame, PreviewState, RESIZE_DEBOUNCE_TICKS};
use crate::search::{SearchHit, SearchState};
use crate::tree::{LoadState, NodeId, NodeKind, TreeReq, TreeState};
use crate::ui;

/// Ticks (100 ms) a toast stays visible.
const TOAST_TICKS: u8 = 30;
/// Rows a tree/inspector PageUp/PageDown moves.
const PAGE_JUMP: usize = 10;
/// Maximum jump-history depth.
const HISTORY_CAP: usize = 64;

/// Which pane has focus (Tab cycles).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Tree,
    Inspector,
    Hex,
}

/// Everything that can happen to the app: terminal input, timer ticks and
/// background-task completions. `App::update` consumes exactly these.
#[derive(Debug)]
pub enum Msg {
    /// A key press from the crossterm event stream.
    Key(KeyEvent),
    /// Terminal resized to `(width, height)` cells.
    Resize(u16, u16),
    /// 100 ms heartbeat: spinner, toast expiry, resize debounce.
    Tick,
    /// A batch of streamed elements for a lazily populated tree section.
    TreeBatch {
        req: TreeReq,
        elements: Vec<Element>,
        /// Elements the stream yielded as errors (salvage: skipped, counted).
        errors: usize,
        done: bool,
    },
    /// A tree population task failed outright.
    TreeFailed { req: TreeReq, error: String },
    /// A page's `/Contents` refs arrived.
    ContentsLoaded { page: usize, refs: Vec<ObjRef> },
    /// Fetching a page's `/Contents` failed.
    ContentsFailed { page: usize, error: String },
    /// The selected element's data arrived for the inspector.
    InspectorLoaded { generation: u64, payload: InspectorPayload },
    /// Loading the inspector payload failed.
    InspectorFailed { generation: u64, error: String },
    /// A window of bytes for the hex pane.
    HexLoaded {
        generation: u64,
        window_start: u64,
        total_len: u64,
        bytes: Vec<u8>,
    },
    /// Reading hex bytes failed.
    HexFailed { generation: u64, error: String },
    /// One incremental search hit.
    SearchResult { generation: u64, hit: SearchHit },
    /// The search task visited every object.
    SearchDone { generation: u64 },
    /// A page preview render finished (or failed).
    PreviewReady {
        generation: u64,
        result: Result<PreviewFrame, String>,
    },
}

/// Side effects `update` requests; the event loop executes them by
/// spawning tasks against a cloned `AsyncDocument`.
#[derive(Debug, Clone)]
pub enum Cmd {
    /// Stream elements to populate a tree section.
    LoadTree(TreeReq),
    /// Fetch page `page`'s dict and extract its `/Contents` refs.
    LoadContents { page: usize, r: ObjRef },
    /// Fetch an object for the inspector.
    LoadObject { generation: u64, r: ObjRef },
    /// Decode the shown stream for the Decoded/Ops inspector views.
    DecodeStream { generation: u64, r: ObjRef },
    /// Load one hex window from the source.
    LoadHex {
        generation: u64,
        source: HexSource,
        window_start: u64,
    },
    /// Start (or restart) an incremental search for `query`.
    StartSearch { generation: u64, query: String },
    /// Render page `page` at fit-to-`(max_w, max_h)`-pixels scale.
    RenderPreview {
        generation: u64,
        page: usize,
        max_w: u32,
        max_h: u32,
        /// Cached whole-file bytes from an earlier render, if any.
        file_bytes: Option<Arc<Vec<u8>>>,
    },
}

/// The whole TUI state.
pub struct App {
    pub title: String,
    pub tree: TreeState,
    pub inspector: InspectorState,
    pub hex: HexState,
    pub preview: PreviewState,
    pub search: SearchState,
    pub focus: Pane,
    pub history: Vec<NodeId>,
    pub pending_jump: Option<ObjRef>,
    pub toast: Option<String>,
    toast_ticks: u8,
    pub size: (u16, u16),
    pub should_quit: bool,
    pub inspector_generation: u64,
    pub hex_generation: u64,
    last_selected: Option<NodeId>,
}

impl App {
    pub fn new(title: String, version: (u8, u8), page_count: usize, size: (u16, u16)) -> App {
        let mut app = App {
            title,
            tree: TreeState::new(version, page_count),
            inspector: InspectorState::new(),
            hex: HexState::new(),
            preview: PreviewState::new(),
            search: SearchState::new(),
            focus: Pane::Tree,
            history: Vec::new(),
            pending_jump: None,
            toast: None,
            toast_ticks: 0,
            size,
            should_quit: false,
            inspector_generation: 0,
            hex_generation: 0,
            last_selected: None,
        };
        let startup = app.on_select(true);
        assert!(startup.is_empty(), "the Document root needs no fetches");
        app
    }

    /// Shows a transient status-bar message.
    pub fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some(message.into());
        self.toast_ticks = TOAST_TICKS;
    }

    /// The status-bar text: search input > toast > breadcrumb + hints.
    pub fn status_line(&self) -> String {
        if self.search.active {
            return self.search.status_line();
        }
        if let Some(message) = &self.toast {
            return message.clone();
        }
        format!(
            "{} \u{b7} {} \u{b7} [/] search  [p] preview  [q] quit",
            self.title,
            self.tree.breadcrumb()
        )
    }

    /// Consumes one message, mutating state and returning side effects.
    pub fn update(&mut self, msg: Msg) -> Vec<Cmd> {
        match msg {
            Msg::Key(key) => self.on_key(key),
            Msg::Resize(width, height) => {
                self.size = (width, height);
                if self.preview.active {
                    self.preview.debounce = Some(RESIZE_DEBOUNCE_TICKS);
                }
                Vec::new()
            }
            Msg::Tick => self.on_tick(),
            Msg::TreeBatch {
                req,
                elements,
                errors,
                done,
            } => {
                self.tree.apply_batch(req, &elements, done);
                if errors > 0 {
                    self.toast(format!("{errors} element(s) unreadable, skipped"));
                }
                let mut cmds = Vec::new();
                if done {
                    if let Some(r) = self.pending_jump.take() {
                        cmds.extend(self.jump_to(r));
                    }
                    cmds.extend(self.on_select(true));
                }
                cmds
            }
            Msg::TreeFailed { req, error } => {
                self.tree.mark_failed(req);
                self.toast(format!("load failed: {error}"));
                Vec::new()
            }
            Msg::ContentsLoaded { page, refs } => {
                self.tree.apply_contents(page, &refs);
                Vec::new()
            }
            Msg::ContentsFailed { page, error } => {
                self.tree.mark_failed(TreeReq::Contents { page });
                self.toast(format!("contents of page {}: {error}", page + 1));
                Vec::new()
            }
            Msg::InspectorLoaded {
                generation,
                payload,
            } => {
                if generation == self.inspector_generation {
                    match payload {
                        InspectorPayload::Object { r, object } => {
                            self.inspector.set_object(r, object)
                        }
                        InspectorPayload::Decoded { r, data } => {
                            self.inspector.set_decoded(r, data)
                        }
                    }
                }
                Vec::new()
            }
            Msg::InspectorFailed { generation, error } => {
                if generation == self.inspector_generation {
                    let title = self.inspector.title.clone();
                    self.inspector
                        .show_message(&title, vec![format!("error: {error}")]);
                    self.toast(error);
                }
                Vec::new()
            }
            Msg::HexLoaded {
                generation,
                window_start,
                total_len,
                bytes,
            } => {
                if generation == self.hex_generation {
                    self.hex.apply_loaded(window_start, total_len, bytes);
                }
                Vec::new()
            }
            Msg::HexFailed { generation, error } => {
                if generation == self.hex_generation {
                    self.hex.loading = false;
                    self.hex.error = Some(error.clone());
                    self.toast(error);
                }
                Vec::new()
            }
            Msg::SearchResult { generation, hit } => {
                self.search.add_hit(generation, hit);
                Vec::new()
            }
            Msg::SearchDone { generation } => {
                self.search.finish(generation);
                Vec::new()
            }
            Msg::PreviewReady { generation, result } => {
                if self.preview.apply_ready(generation, result) {
                    if let Some(error) = self.preview.error.clone() {
                        self.toast(format!("preview: {error}"));
                    }
                }
                Vec::new()
            }
        }
    }

    fn on_tick(&mut self) -> Vec<Cmd> {
        if self.toast_ticks > 0 {
            self.toast_ticks -= 1;
            if self.toast_ticks == 0 {
                self.toast = None;
            }
        }
        if self.preview.tick() {
            return self.request_preview();
        }
        Vec::new()
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        let action = action_for(key, self.search.active);
        self.on_action(action)
    }

    fn on_action(&mut self, action: Action) -> Vec<Cmd> {
        match action {
            Action::Noop => Vec::new(),
            Action::Quit => {
                self.should_quit = true;
                Vec::new()
            }
            Action::OpenSearch => {
                self.search.open();
                Vec::new()
            }
            Action::SearchChar(c) => {
                let generation = self.search.push_char(c);
                self.start_search(generation)
            }
            Action::SearchBackspace => match self.search.pop_char() {
                Some(generation) => self.start_search(generation),
                None => Vec::new(),
            },
            Action::SearchAccept => {
                self.search.accept();
                Vec::new()
            }
            Action::SearchCancel => {
                self.search.cancel();
                Vec::new()
            }
            Action::NextHit => match self.search.next_hit() {
                Some(hit) => self.jump_to(hit.r),
                None => Vec::new(),
            },
            Action::PrevHit => match self.search.prev_hit() {
                Some(hit) => self.jump_to(hit.r),
                None => Vec::new(),
            },
            Action::FocusNext => {
                self.focus = match self.focus {
                    Pane::Tree => Pane::Inspector,
                    Pane::Inspector => Pane::Hex,
                    Pane::Hex => Pane::Tree,
                };
                Vec::new()
            }
            Action::TogglePreview => {
                if self.preview.active {
                    self.preview.active = false;
                    Vec::new()
                } else {
                    self.preview.active = true;
                    self.request_preview()
                }
            }
            Action::CycleView => {
                if !self.inspector.is_stream() {
                    if self.inspector.object.is_some() {
                        self.toast("d: not a stream");
                    }
                    return Vec::new();
                }
                let needs_decode = self.inspector.cycle_mode();
                match (needs_decode, self.inspector.object.as_ref()) {
                    (true, Some((r, ..))) => vec![Cmd::DecodeStream {
                        generation: self.inspector_generation,
                        r: *r,
                    }],
                    (true, None) | (false, ..) => Vec::new(),
                }
            }
            Action::Back => match self.history.pop() {
                Some(id) => {
                    self.tree.reveal(id);
                    self.tree.selected = id;
                    self.on_select(false)
                }
                None => {
                    self.toast("history empty");
                    Vec::new()
                }
            },
            Action::Activate => self.on_activate(),
            Action::MoveUp => self.on_move(-1),
            Action::MoveDown => self.on_move(1),
            Action::PageUp => self.on_page(-1),
            Action::PageDown => self.on_page(1),
            Action::Collapse => match self.focus {
                Pane::Tree => {
                    self.tree.collapse_or_parent(self.tree.selected);
                    self.on_select(false)
                }
                Pane::Inspector | Pane::Hex => Vec::new(),
            },
            Action::Expand => match self.focus {
                Pane::Tree => self.expand_selected(),
                Pane::Inspector | Pane::Hex => Vec::new(),
            },
            Action::Top => match self.focus {
                Pane::Tree => {
                    self.tree.select_top();
                    self.on_select(false)
                }
                Pane::Inspector => {
                    self.inspector.scroll = 0;
                    self.inspector.ref_cursor = None;
                    Vec::new()
                }
                Pane::Hex => {
                    self.hex.scroll_to(0);
                    self.ensure_hex_window()
                }
            },
            Action::Bottom => match self.focus {
                Pane::Tree => {
                    self.tree.select_bottom();
                    self.on_select(false)
                }
                Pane::Inspector => {
                    self.inspector.scroll =
                        self.inspector.lines.len().saturating_sub(1) as u16;
                    Vec::new()
                }
                Pane::Hex => {
                    let last = self.hex.line_count().saturating_sub(1);
                    self.hex.scroll_to(last);
                    self.ensure_hex_window()
                }
            },
        }
    }

    fn on_move(&mut self, delta: i32) -> Vec<Cmd> {
        match self.focus {
            Pane::Tree => {
                if delta < 0 {
                    self.tree.select_prev();
                } else {
                    self.tree.select_next();
                }
                self.on_select(false)
            }
            Pane::Inspector => {
                self.inspector.move_cursor(delta);
                Vec::new()
            }
            Pane::Hex => {
                self.hex.scroll_by(i64::from(delta));
                self.ensure_hex_window()
            }
        }
    }

    fn on_page(&mut self, direction: i32) -> Vec<Cmd> {
        match self.focus {
            Pane::Tree => {
                let mut remaining = PAGE_JUMP;
                while remaining > 0 {
                    remaining -= 1;
                    if direction < 0 {
                        self.tree.select_prev();
                    } else {
                        self.tree.select_next();
                    }
                }
                self.on_select(false)
            }
            Pane::Inspector => {
                self.inspector.move_cursor(direction * PAGE_JUMP as i32);
                Vec::new()
            }
            Pane::Hex => {
                let rows = i64::from(self.hex_visible_rows());
                self.hex.scroll_by(i64::from(direction) * rows);
                self.ensure_hex_window()
            }
        }
    }

    fn on_activate(&mut self) -> Vec<Cmd> {
        match self.focus {
            Pane::Tree => match self.tree.selection_ref(self.tree.selected) {
                Some(r) => self.jump_to(r),
                None => self.expand_selected(),
            },
            Pane::Inspector => match self.inspector.current_ref() {
                Some(r) => self.jump_to(r),
                None => Vec::new(),
            },
            Pane::Hex => Vec::new(),
        }
    }

    /// Jumps to object `r` in the Objects folder, recording history; if
    /// the physical pass has not run yet, defers the jump behind it.
    pub fn jump_to(&mut self, r: ObjRef) -> Vec<Cmd> {
        if let Some(id) = self.tree.find_object(r) {
            self.history.push(self.tree.selected);
            if self.history.len() > HISTORY_CAP {
                self.history.remove(0);
            }
            self.tree.reveal(id);
            self.tree.selected = id;
            self.focus = Pane::Tree;
            return self.on_select(true);
        }
        match self.tree.physical {
            LoadState::NotLoaded => {
                self.tree.physical = LoadState::Loading;
                self.pending_jump = Some(r);
                vec![Cmd::LoadTree(TreeReq::Physical)]
            }
            LoadState::Loading => {
                self.pending_jump = Some(r);
                Vec::new()
            }
            LoadState::Loaded | LoadState::Failed => {
                self.toast(format!("object {} {} R not found", r.num, r.gen));
                Vec::new()
            }
        }
    }

    fn start_search(&mut self, generation: u64) -> Vec<Cmd> {
        if self.search.query.is_empty() {
            return Vec::new();
        }
        vec![Cmd::StartSearch {
            generation,
            query: self.search.query.clone(),
        }]
    }

    fn expand_selected(&mut self) -> Vec<Cmd> {
        let id = self.tree.selected;
        // On an already-expanded branch, descend to the first child.
        if self.tree.is_branch(id)
            && self.tree.node(id).expanded
            && !self.tree.node(id).children.is_empty()
        {
            self.tree.selected = self.tree.node(id).children[0];
            return self.on_select(false);
        }
        match self.tree.expand(id) {
            Some(TreeReq::Contents { page }) => match self.tree.page_ref(page) {
                Some(r) => vec![Cmd::LoadContents { page, r }],
                None => {
                    self.toast("page object unknown");
                    Vec::new()
                }
            },
            Some(req) => vec![Cmd::LoadTree(req)],
            None => Vec::new(),
        }
    }

    /// Reloads the inspector and hex panes for the current selection.
    /// `force` refreshes even when the selection did not change (used
    /// after tree loads complete).
    fn on_select(&mut self, force: bool) -> Vec<Cmd> {
        if !force && self.last_selected == Some(self.tree.selected) {
            return Vec::new();
        }
        self.last_selected = Some(self.tree.selected);
        self.inspector_generation += 1;
        self.hex_generation += 1;
        let id = self.tree.selected;
        let kind = self.tree.node(id).kind.clone();
        let mut cmds = Vec::new();
        match kind {
            NodeKind::Document => {
                self.inspector.show_message(
                    "Document",
                    vec![
                        format!(
                            "version: {}.{}",
                            self.tree.version.0, self.tree.version.1
                        ),
                        format!("pages: {}", self.tree.page_count),
                    ],
                );
                self.hex.clear();
            }
            NodeKind::Object { r, span, in_objstm } => {
                self.inspector
                    .show_loading(&format!("obj {} {}", r.num, r.gen));
                cmds.push(Cmd::LoadObject {
                    generation: self.inspector_generation,
                    r,
                });
                cmds.extend(self.hex_for(span, in_objstm));
            }
            NodeKind::Page { r, .. }
            | NodeKind::Font { r, .. }
            | NodeKind::Image { r, .. }
            | NodeKind::Annotation { r, .. }
            | NodeKind::ContentsStream { r } => {
                self.inspector
                    .show_loading(&format!("obj {} {}", r.num, r.gen));
                cmds.push(Cmd::LoadObject {
                    generation: self.inspector_generation,
                    r,
                });
                match self.tree.object_span(r.num) {
                    Some((span, in_objstm)) => cmds.extend(self.hex_for(span, in_objstm)),
                    None => {
                        self.hex.clear();
                        if self.tree.physical == LoadState::NotLoaded {
                            self.tree.physical = LoadState::Loading;
                            cmds.push(Cmd::LoadTree(TreeReq::Physical));
                        }
                    }
                }
            }
            NodeKind::XrefSection { kind, span, entries } => {
                let name = match kind {
                    pdfboss_core::elements::XrefKind::Table => "xref table",
                    pdfboss_core::elements::XrefKind::Stream => "xref stream",
                };
                self.inspector.show_message(
                    name,
                    vec![
                        format!("entries: {entries}"),
                        format!("span: {:#x}..{:#x}", span.start, span.end),
                    ],
                );
                cmds.extend(self.hex_file(span));
            }
            NodeKind::StartXref { offset, span } => {
                self.inspector.show_message(
                    "startxref",
                    vec![format!("offset: {offset} ({offset:#x})")],
                );
                cmds.extend(self.hex_file(span));
            }
            NodeKind::Eof { span } => {
                self.inspector
                    .show_message("%%EOF", vec!["end-of-file marker".to_string()]);
                cmds.extend(self.hex_file(span));
            }
            NodeKind::Trailer => {
                match self.tree.trailer_dict.clone() {
                    Some(dict) => self.inspector.set_dict("Trailer", &dict),
                    None => {
                        self.inspector.show_loading("Trailer");
                        if self.tree.physical == LoadState::NotLoaded {
                            self.tree.physical = LoadState::Loading;
                            cmds.push(Cmd::LoadTree(TreeReq::Physical));
                        }
                    }
                }
                match self.tree.trailer_span {
                    Some(span) => cmds.extend(self.hex_file(span)),
                    None => self.hex.clear(),
                }
            }
            NodeKind::PagesFolder
            | NodeKind::FontsFolder { .. }
            | NodeKind::ImagesFolder { .. }
            | NodeKind::AnnotationsFolder { .. }
            | NodeKind::ContentsFolder { .. }
            | NodeKind::ObjectsFolder
            | NodeKind::XrefFolder => {
                let label = self.tree.label(id);
                self.inspector.show_message(&label, Vec::new());
                self.hex.clear();
            }
        }
        cmds
    }

    fn hex_for(&mut self, span: Span, in_objstm: Option<(ObjRef, Span)>) -> Vec<Cmd> {
        match in_objstm {
            Some((container, member)) => {
                let source = HexSource::DecodedObjStm { container };
                self.hex.set_source(source.clone());
                self.hex.highlight = Some(member);
                vec![Cmd::LoadHex {
                    generation: self.hex_generation,
                    source,
                    window_start: 0,
                }]
            }
            None => self.hex_file(span),
        }
    }

    fn hex_file(&mut self, span: Span) -> Vec<Cmd> {
        let source = HexSource::File { span };
        self.hex.set_source(source.clone());
        vec![Cmd::LoadHex {
            generation: self.hex_generation,
            source,
            window_start: 0,
        }]
    }

    fn ensure_hex_window(&mut self) -> Vec<Cmd> {
        let rows = self.hex_visible_rows();
        match (self.hex.visible_window_missing(rows), self.hex.source.clone()) {
            (Some(window_start), Some(source)) => {
                self.hex.loading = true;
                self.hex_generation += 1;
                vec![Cmd::LoadHex {
                    generation: self.hex_generation,
                    source,
                    window_start,
                }]
            }
            (Some(..), None) | (None, ..) => Vec::new(),
        }
    }

    fn request_preview(&mut self) -> Vec<Cmd> {
        if self.tree.page_count == 0 {
            self.toast("document has no pages to preview");
            self.preview.active = false;
            return Vec::new();
        }
        let page = self.tree.page_of(self.tree.selected).unwrap_or(0);
        let generation = self.preview.start_render(page);
        let (max_w, max_h) = self.preview_budget();
        vec![Cmd::RenderPreview {
            generation,
            page,
            max_w,
            max_h,
            file_bytes: self.preview.file_bytes.clone(),
        }]
    }

    fn preview_budget(&self) -> (u32, u32) {
        let area = ratatui::layout::Rect::new(0, 0, self.size.0, self.size.1);
        let split = ui::panes(area);
        let width = u32::from(split.right_top.width.saturating_sub(2)).max(1);
        // Two vertical pixels per cell row (`▀` half-blocks).
        let height = (u32::from(split.right_top.height.saturating_sub(2)) * 2).max(1);
        (width, height)
    }

    fn hex_visible_rows(&self) -> u16 {
        let area = ratatui::layout::Rect::new(0, 0, self.size.0, self.size.1);
        ui::panes(area).hex.height.saturating_sub(2).max(1)
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui app` — expect all 9 tests green. (The `PageDown` expectation in `hex_scrolling_requests_missing_windows`: the trailer span is 40 bytes = 5 lines; hex pane rows at 80x24 = 7; scrolling down 7 clamps to line 4.)
- [ ] Run clippy + fmt — expect clean.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): app state machine with msg/cmd update loop"`

---

### Task 9: Rendering + TestBackend snapshot tests (`ui.rs`, `tests/snapshots.rs`)

**Files:**
- Modify: `crates/pdfboss-tui/src/ui.rs` (add rendering below the `panes` function from Task 8)
- Create: `crates/pdfboss-tui/tests/snapshots.rs`
- Test: `crates/pdfboss-tui/tests/snapshots.rs` plus the existing `ui::tests`

**Interfaces:**
- Consumes: everything from Tasks 2–8; `pdfboss_testkit::simple_doc(text: &str) -> Vec<u8>` (existing, `crates/pdfboss-testkit/src/lib.rs:253`); sync `pdfboss_core::Document::{load, get, version, page_count}` (existing, `crates/pdfboss-core/src/document.rs`); `Document::elements(&self, opts: ElementOpts) -> Elements<'_>` (plan 01).
- Produces: `ui::draw(app: &App, frame: &mut Frame)`.

**Note on determinism:** snapshots run on a fixed 80x24 `TestBackend` and compare **symbols only** (styles are exercised by the unit tests of Tasks 3/5/6, which inspect `Style` directly). Frames never display byte offsets that plan 01 chooses (frame A shows no hex; frame C feeds a hand-built element batch with test-chosen spans over real fixture bytes), so the expected strings are fully derivable. Both sides are compared through `trim_end()` — trailing padding is the only non-load-bearing whitespace.

**Steps:**

- [ ] Write the failing snapshot tests. Create `crates/pdfboss-tui/tests/snapshots.rs`:

```rust
//! Full-frame snapshot tests on a fixed 80x24 TestBackend: tree render,
//! inspector dict, hex pane and status bar over a testkit fixture.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pdfboss_core::elements::{Element, ElementOpts, Span};
use pdfboss_core::{Document, Object, ObjRef};
use pdfboss_tui::app::{App, Msg};
use pdfboss_tui::tree::TreeReq;
use pdfboss_tui::ui;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn assert_frame(terminal: &Terminal<TestBackend>, expected: &[&str]) {
    let lines = buffer_lines(terminal);
    assert_eq!(lines.len(), expected.len(), "frame height");
    for (index, want) in expected.iter().enumerate() {
        assert_eq!(
            lines[index].trim_end(),
            want.trim_end(),
            "frame line {index}"
        );
    }
}

fn draw(app: &App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
    terminal.draw(|frame| ui::draw(app, frame)).expect("draw");
    terminal
}

/// Frame A: document overview after the physical pass — tree with counts,
/// document summary in the inspector, empty hex pane, breadcrumb status.
#[test]
fn document_overview_frame() {
    let data = pdfboss_testkit::simple_doc("Hello");
    let doc = Document::load(data).expect("fixture loads");
    let elements: Vec<Element> = doc
        .elements(ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        })
        .filter_map(Result::ok)
        .collect();
    let mut app = App::new(
        "fixture.pdf".to_string(),
        doc.version(),
        doc.page_count(),
        (80, 24),
    );
    app.update(Msg::TreeBatch {
        req: TreeReq::Physical,
        elements,
        errors: 0,
        done: true,
    });
    let terminal = draw(&app);
    assert_frame(
        &terminal,
        &[
            "┌Tree──────────────────────┐┌Inspector · Document──────────────────────────────┐",
            "│▾ Document · PDF 1.7      ││version: 1.7                                      │",
            "│  ▸ Pages (1)             ││pages: 1                                          │",
            "│  ▸ Objects (5)           ││                                                  │",
            "│  ▸ Xref (1 secs)         ││                                                  │",
            "│    Trailer               ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          │└──────────────────────────────────────────────────┘",
            "│                          │┌Hex───────────────────────────────────────────────┐",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "└──────────────────────────┘└──────────────────────────────────────────────────┘",
            "fixture.pdf · /Document · [/] search  [p] preview  [q] quit",
        ],
    );
}

/// Frame C: an object selected — expanded tree, catalog dict in the
/// inspector, its bytes hexdumped, breadcrumb status. The element batch is
/// hand-built with test-chosen spans (0..15 header, 15..64 object 1) so the
/// hex gutter is static; the dumped bytes are the real fixture bytes.
#[test]
fn object_inspection_frame() {
    let data = pdfboss_testkit::simple_doc("Hello");
    let doc = Document::load(data.clone()).expect("fixture loads");
    let catalog = doc.get(ObjRef { num: 1, gen: 0 }).expect("object 1");
    let elements = vec![
        Element::Header {
            version: (1, 7),
            span: Span { start: 0, end: 15 },
        },
        Element::IndirectObject {
            r: ObjRef { num: 1, gen: 0 },
            object: Object::Null,
            span: Span { start: 15, end: 64 },
            in_objstm: None,
        },
    ];
    let mut app = App::new("fixture.pdf".to_string(), (1, 7), 1, (80, 24));
    app.update(Msg::TreeBatch {
        req: TreeReq::Physical,
        elements,
        errors: 0,
        done: true,
    });
    app.update(key(KeyCode::Char('j'))); // Pages
    app.update(key(KeyCode::Char('j'))); // Objects
    app.update(key(KeyCode::Char('l'))); // expand Objects
    app.update(key(KeyCode::Char('j'))); // obj 1 0
    app.update(Msg::InspectorLoaded {
        generation: app.inspector_generation,
        payload: pdfboss_tui::inspector::InspectorPayload::Object {
            r: ObjRef { num: 1, gen: 0 },
            object: catalog,
        },
    });
    app.update(Msg::HexLoaded {
        generation: app.hex_generation,
        window_start: 0,
        total_len: 49,
        bytes: data[15..64].to_vec(),
    });
    let terminal = draw(&app);
    assert_frame(
        &terminal,
        &[
            "┌Tree──────────────────────┐┌Inspector · obj 1 0───────────────────────────────┐",
            "│▾ Document · PDF 1.7      ││<<                                                │",
            "│  ▸ Pages (1)             ││  /Pages 2 0 R                                    │",
            "│  ▾ Objects (1)           ││  /Type /Catalog                                  │",
            "│      obj 1 0             ││>>                                                │",
            "│  ▸ Xref (0 secs)         ││                                                  │",
            "│    Trailer               ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          │└──────────────────────────────────────────────────┘",
            "│                          │┌Hex 0xf..0x40─────────────────────────────────────┐",
            "│                          ││0000000f │ 31 20 30 20 6f 62 6a 0a │ 1 0 obj·     │",
            "│                          ││00000017 │ 3c 3c 20 2f 54 79 70 65 │ << /Type     │",
            "│                          ││0000001f │ 20 2f 43 61 74 61 6c 6f │  /Catalo     │",
            "│                          ││00000027 │ 67 20 2f 50 61 67 65 73 │ g /Pages     │",
            "│                          ││0000002f │ 20 32 20 30 20 52 20 3e │  2 0 R >     │",
            "│                          ││00000037 │ 3e 0a 65 6e 64 6f 62 6a │ >·endobj     │",
            "│                          ││0000003f │ 0a                      │ ·            │",
            "└──────────────────────────┘└──────────────────────────────────────────────────┘",
            "fixture.pdf · /Document/Objects/obj 1 0 · [/] search  [p] preview  [q] quit",
        ],
    );
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui --test snapshots` — expect a compile error: `ui::draw` does not exist yet.
- [ ] Implement rendering. Append to `crates/pdfboss-tui/src/ui.rs` (below `panes`, above the test module), and extend the file's imports to exactly:

```rust
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::{App, Pane};
use crate::hexview::{hex_line, highlight_cols, BYTES_PER_LINE};
use crate::inspector::InspectorMode;
use crate::preview::{cell_colors, SPINNER};
```

```rust
/// Renders the whole app into one frame.
pub fn draw(app: &App, frame: &mut Frame) {
    let split = panes(frame.area());
    draw_tree(app, frame, split.tree);
    if app.preview.active {
        draw_preview(app, frame, split.right_top);
    } else {
        draw_inspector(app, frame, split.right_top);
    }
    draw_hex(app, frame, split.hex);
    draw_status(app, frame, split.status);
}

fn pane_block(title: String, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title);
    if focused {
        block.title_style(Style::default().add_modifier(Modifier::BOLD))
    } else {
        block
    }
}

fn draw_tree(app: &App, frame: &mut Frame, area: Rect) {
    let block = pane_block("Tree".to_string(), app.focus == Pane::Tree);
    let inner_height = area.height.saturating_sub(2) as usize;
    let rows = app.tree.visible_rows();
    let selected_position = rows
        .iter()
        .position(|row| row.id == app.tree.selected)
        .unwrap_or(0);
    let offset = selected_position.saturating_sub(inner_height.saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();
    for row in rows.iter().skip(offset).take(inner_height) {
        let glyph = if app.tree.is_branch(row.id) {
            if app.tree.node(row.id).expanded {
                "\u{25be} "
            } else {
                "\u{25b8} "
            }
        } else {
            "  "
        };
        let text = format!(
            "{}{}{}",
            "  ".repeat(row.depth),
            glyph,
            app.tree.label(row.id)
        );
        let style = if row.id == app.tree.selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::styled(text, style));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn draw_inspector(app: &App, frame: &mut Frame, area: Rect) {
    let mode_suffix = if app.inspector.is_stream() && app.inspector.mode != InspectorMode::Pretty
    {
        format!(" [{}]", app.inspector.mode_name())
    } else {
        String::new()
    };
    let title = if app.inspector.title.is_empty() {
        "Inspector".to_string()
    } else {
        format!("Inspector \u{b7} {}{}", app.inspector.title, mode_suffix)
    };
    let block = pane_block(title, app.focus == Pane::Inspector);
    let cursor_line = app
        .inspector
        .ref_cursor
        .and_then(|index| app.inspector.refs.get(index))
        .map(|(line, ..)| *line);
    let lines: Vec<Line> = app
        .inspector
        .lines
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let style = if Some(index) == cursor_line {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::styled(text.clone(), style)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((app.inspector.scroll, 0))
            .block(block),
        area,
    );
}

fn draw_hex(app: &App, frame: &mut Frame, area: Rect) {
    let block = pane_block(app.hex.title(), app.focus == Pane::Hex);
    let inner_height = u64::from(area.height.saturating_sub(2));
    let mut lines: Vec<Line> = Vec::new();
    if let Some(error) = &app.hex.error {
        lines.push(Line::raw(format!("error: {error}")));
    } else if app.hex.loading {
        lines.push(Line::raw("loading\u{2026}"));
    } else if app.hex.source.is_some() {
        let mut row = 0u64;
        while row < inner_height {
            let line_index = app.hex.scroll_line + row;
            if line_index >= app.hex.line_count() {
                break;
            }
            let offset = line_index * BYTES_PER_LINE as u64;
            let end = (offset + BYTES_PER_LINE as u64).min(app.hex.total_len);
            let window_end = app.hex.window_start + app.hex.bytes.len() as u64;
            if offset < app.hex.window_start || end > window_end {
                // Bytes outside the resident window (a fetch is in flight).
                lines.push(Line::raw("\u{2026}"));
            } else {
                let first = (offset - app.hex.window_start) as usize;
                let last = (end - app.hex.window_start) as usize;
                let slice = &app.hex.bytes[first..last];
                let hl = app
                    .hex
                    .highlight
                    .and_then(|span| highlight_cols(offset, slice.len(), span));
                lines.push(hex_line(app.hex.base + offset, slice, hl));
            }
            row += 1;
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn draw_preview(app: &App, frame: &mut Frame, area: Rect) {
    let title = match app.preview.page {
        Some(page) => format!("Preview \u{b7} page {}", page + 1),
        None => "Preview".to_string(),
    };
    let block = pane_block(title, app.focus == Pane::Inspector);
    let inner_width = u32::from(area.width.saturating_sub(2));
    let inner_height = u32::from(area.height.saturating_sub(2));
    let mut lines: Vec<Line> = Vec::new();
    if app.preview.rendering {
        lines.push(Line::raw(format!(
            "{} rendering\u{2026}",
            SPINNER[app.preview.spinner_frame]
        )));
    } else if let Some(error) = &app.preview.error {
        lines.push(Line::raw(format!("error: {error}")));
    } else if let Some(pix) = &app.preview.pixmap {
        let columns = pix.width.min(inner_width);
        let rows = pix.height.div_ceil(2).min(inner_height);
        let mut row = 0u32;
        while row < rows {
            let mut cells: Vec<Span> = Vec::new();
            let mut column = 0u32;
            while column < columns {
                let (fg, bg) = cell_colors(pix, column, row);
                cells.push(Span::styled("\u{2580}", Style::default().fg(fg).bg(bg)));
                column += 1;
            }
            lines.push(Line::from(cells));
            row += 1;
        }
    } else {
        lines.push(Line::raw("no preview yet"));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn draw_status(app: &App, frame: &mut Frame, area: Rect) {
    frame.render_widget(Paragraph::new(app.status_line()), area);
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui` — expect the full crate suite green, including both snapshots. If a snapshot line differs, diff the printed actual vs expected line; legitimate divergence can only come from plan-01's physical element stream shape in frame A (e.g. object count if the fixture gains objects) — fix the expected string to the observed truth only after confirming the underlying label logic is per this plan.
- [ ] Run clippy + fmt — expect clean.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): frame rendering with TestBackend snapshots"`

---

### Task 10: Event loop and command executor (`lib.rs run()`)

**Files:**
- Modify: `crates/pdfboss-tui/src/lib.rs` (full file shown below)
- Test: unit test inside `crates/pdfboss-tui/src/lib.rs`; manual smoke deferred to Task 11 (the subcommand lands there)

**Interfaces:**
- Consumes (plan 02, spec-pinned plus the cross-review addition): `AsyncDocument::{version() -> (u8, u8), page_count() -> usize, file_len() -> u64 (sync, available immediately after open), get_object(ObjRef) -> Result<Object>, decode_stream(&Stream) -> Result<Vec<u8>>, read_span(Span) -> Result<Vec<u8>>, elements(ElementOpts) -> ElementStream<'_>}` with `AsyncDocument: Send + Sync + Clone` and `ElementStream: futures_core::Stream<Item = Result<Element>> + Send`; `pdfboss_render::render_page(doc: &Document, page: &Page, scale: f32) -> Result<Pixmap>` (existing, `crates/pdfboss-render/src/lib.rs:168`); sync `Document::load(Vec<u8>)`, `Document::page(usize) -> Result<Page>`, `Page::size() -> (f32, f32)` (existing core).
- Produces: `pub async fn run(doc: AsyncDocument, title: String) -> std::io::Result<()>` — the crate's whole public entry point per the spec.

**Steps:**

- [ ] Replace `crates/pdfboss-tui/src/lib.rs` with the full file (module declarations from Tasks 2–9 plus the event loop):

```rust
//! Interactive terminal explorer for PDF internals, implemented from
//! ISO 32000 on top of `pdfboss-aio`'s async document model.
//!
//! State machine (`app`), pane models (`tree`, `inspector`, `hexview`,
//! `preview`, `search`), key mapping (`input`) and rendering (`ui`) are
//! pure and unit-testable; only [`run`] touches the real terminal. The
//! event loop `tokio::select!`s over the crossterm event stream, a
//! background-task message channel and a 100 ms tick, so long operations
//! (element streaming, hex fetches, search, preview rasterization) never
//! block input.

pub mod app;
pub mod hexview;
pub mod input;
pub mod inspector;
pub mod preview;
pub mod search;
pub mod tree;
pub mod ui;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use pdfboss_aio::AsyncDocument;
use pdfboss_core::elements::{Element, ElementOpts, Span};
use pdfboss_core::ObjRef;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::{App, Cmd, Msg};
use crate::hexview::{HexSource, WINDOW_BYTES};
use crate::inspector::InspectorPayload;
use crate::preview::{fit_scale, PreviewFrame};
use crate::search::{object_matches, SearchHit};
use crate::tree::TreeReq;

/// Elements per tree batch message.
const TREE_BATCH: usize = 64;

/// Restores the terminal on drop, so panics and early returns never leave
/// the shell in raw mode.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        disable_raw_mode().ok();
        execute!(std::io::stdout(), LeaveAlternateScreen).ok();
    }
}

/// Runs the explorer until the user quits. `doc` supplies all data (file-
/// or HTTP-backed); `title` labels the status bar. Document-level errors
/// become status-bar toasts; only terminal I/O errors are returned.
pub async fn run(doc: AsyncDocument, title: String) -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let guard = TerminalGuard;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let size = terminal.size()?;
    let mut app = App::new(
        title,
        doc.version(),
        doc.page_count(),
        (size.width, size.height),
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let search_epoch = Arc::new(AtomicU64::new(0));
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        terminal.draw(|frame| ui::draw(&app, frame))?;
        let msg = tokio::select! {
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) => Msg::Key(key),
                Some(Ok(Event::Resize(width, height))) => Msg::Resize(width, height),
                Some(Ok(..)) => continue,
                Some(Err(..)) | None => break,
            },
            Some(msg) = rx.recv() => msg,
            _ = tick.tick() => Msg::Tick,
        };
        for cmd in app.update(msg) {
            execute_cmd(&doc, &tx, &search_epoch, cmd);
        }
        if app.should_quit {
            break;
        }
    }
    drop(guard);
    Ok(())
}

/// Spawns the background task a [`Cmd`] describes; completions come back
/// to the loop as [`Msg`]s on the channel.
fn execute_cmd(
    doc: &AsyncDocument,
    tx: &UnboundedSender<Msg>,
    search_epoch: &Arc<AtomicU64>,
    cmd: Cmd,
) {
    let doc = doc.clone();
    let tx = tx.clone();
    match cmd {
        Cmd::LoadTree(req) => {
            tokio::spawn(load_tree(doc, tx, req));
        }
        Cmd::LoadContents { page, r } => {
            tokio::spawn(load_contents(doc, tx, page, r));
        }
        Cmd::LoadObject { generation, r } => {
            tokio::spawn(async move {
                let message = match doc.get_object(r).await {
                    Ok(object) => Msg::InspectorLoaded {
                        generation,
                        payload: InspectorPayload::Object { r, object },
                    },
                    Err(error) => Msg::InspectorFailed {
                        generation,
                        error: error.to_string(),
                    },
                };
                tx.send(message).ok();
            });
        }
        Cmd::DecodeStream { generation, r } => {
            tokio::spawn(async move {
                let message = match decoded_stream_data(&doc, r).await {
                    Ok(data) => Msg::InspectorLoaded {
                        generation,
                        payload: InspectorPayload::Decoded { r, data },
                    },
                    Err(error) => Msg::InspectorFailed { generation, error },
                };
                tx.send(message).ok();
            });
        }
        Cmd::LoadHex {
            generation,
            source,
            window_start,
        } => {
            tokio::spawn(load_hex(doc, tx, generation, source, window_start));
        }
        Cmd::StartSearch { generation, query } => {
            let epoch = Arc::clone(search_epoch);
            epoch.store(generation, Ordering::SeqCst);
            tokio::spawn(run_search(doc, tx, epoch, generation, query));
        }
        Cmd::RenderPreview {
            generation,
            page,
            max_w,
            max_h,
            file_bytes,
        } => {
            tokio::spawn(render_preview(
                doc, tx, generation, page, max_w, max_h, file_bytes,
            ));
        }
    }
}

/// Streams a tree section's elements in batches. Per-element parse errors
/// are counted, never fatal (salvage semantics: a document with an
/// unusable logical layer still explores physically).
async fn load_tree(doc: AsyncDocument, tx: UnboundedSender<Msg>, req: TreeReq) {
    let opts = match req {
        TreeReq::Physical => ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        },
        TreeReq::Logical => ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: false,
        },
        // Contents folders load through `load_contents`.
        TreeReq::Contents { .. } => return,
    };
    let mut stream = doc.elements(opts);
    let mut batch: Vec<Element> = Vec::new();
    let mut errors = 0usize;
    while let Some(item) = stream.next().await {
        match item {
            Ok(element) => batch.push(element),
            Err(..) => errors += 1,
        }
        if batch.len() >= TREE_BATCH {
            let elements = std::mem::take(&mut batch);
            let batch_errors = std::mem::take(&mut errors);
            let sent = tx.send(Msg::TreeBatch {
                req,
                elements,
                errors: batch_errors,
                done: false,
            });
            if sent.is_err() {
                return;
            }
        }
    }
    tx.send(Msg::TreeBatch {
        req,
        elements: batch,
        errors,
        done: true,
    })
    .ok();
}

/// Fetches a page dict and reports its `/Contents` refs (a single ref or
/// an array of refs).
async fn load_contents(doc: AsyncDocument, tx: UnboundedSender<Msg>, page: usize, r: ObjRef) {
    let message = match page_contents(&doc, r).await {
        Ok(refs) => Msg::ContentsLoaded { page, refs },
        Err(error) => Msg::ContentsFailed { page, error },
    };
    tx.send(message).ok();
}

async fn page_contents(doc: &AsyncDocument, r: ObjRef) -> Result<Vec<ObjRef>, String> {
    let object = doc.get_object(r).await.map_err(|error| error.to_string())?;
    let Some(dict) = object.as_dict() else {
        return Err(format!("object {} {} is not a page dict", r.num, r.gen));
    };
    let mut refs = Vec::new();
    match dict.get("Contents") {
        Some(pdfboss_core::Object::Ref(content_ref)) => refs.push(*content_ref),
        Some(pdfboss_core::Object::Array(items)) => {
            for item in items {
                if let pdfboss_core::Object::Ref(content_ref) = item {
                    refs.push(*content_ref);
                }
            }
        }
        Some(..) | None => {}
    }
    Ok(refs)
}

/// Decoded data of stream object `r`.
async fn decoded_stream_data(doc: &AsyncDocument, r: ObjRef) -> Result<Vec<u8>, String> {
    let object = doc.get_object(r).await.map_err(|error| error.to_string())?;
    let Some(stream) = object.as_stream() else {
        return Err(format!("object {} {} is not a stream", r.num, r.gen));
    };
    doc.decode_stream(stream)
        .await
        .map_err(|error| error.to_string())
}

/// Loads one hex window: a `read_span` window of a file span, or the whole
/// decoded object-stream container (decoded buffers are small).
async fn load_hex(
    doc: AsyncDocument,
    tx: UnboundedSender<Msg>,
    generation: u64,
    source: HexSource,
    window_start: u64,
) {
    let outcome: Result<(u64, u64, Vec<u8>), String> = match source {
        HexSource::File { span } => {
            let total_len = span.end.saturating_sub(span.start);
            let start = span.start + window_start;
            let end = (start + WINDOW_BYTES as u64).min(span.end);
            match doc.read_span(Span { start, end }).await {
                Ok(bytes) => Ok((window_start, total_len, bytes)),
                Err(error) => Err(error.to_string()),
            }
        }
        HexSource::DecodedObjStm { container } => {
            match decoded_stream_data(&doc, container).await {
                Ok(bytes) => Ok((0, bytes.len() as u64, bytes)),
                Err(error) => Err(error),
            }
        }
    };
    let message = match outcome {
        Ok((start, total_len, bytes)) => Msg::HexLoaded {
            generation,
            window_start: start,
            total_len,
            bytes,
        },
        Err(error) => Msg::HexFailed { generation, error },
    };
    tx.send(message).ok();
}

/// Visits physical objects lazily, streaming one message per match. A
/// newer search generation (shared epoch) terminates this task early.
async fn run_search(
    doc: AsyncDocument,
    tx: UnboundedSender<Msg>,
    epoch: Arc<AtomicU64>,
    generation: u64,
    query: String,
) {
    let opts = ElementOpts {
        physical: true,
        logical: false,
        pages: None,
        content_ops: false,
    };
    let mut stream = doc.elements(opts);
    while let Some(item) = stream.next().await {
        if epoch.load(Ordering::SeqCst) != generation {
            return;
        }
        let Ok(Element::IndirectObject { r, object, .. }) = item else {
            continue;
        };
        if object_matches(&query, r.num, &object) {
            let hit = SearchHit { r };
            if tx.send(Msg::SearchResult { generation, hit }).is_err() {
                return;
            }
        }
    }
    tx.send(Msg::SearchDone { generation }).ok();
}

/// Renders a page preview. The whole file is fetched once (and cached by
/// the app for later renders); rasterization runs in `spawn_blocking`, and
/// the sync `Document` is created and dropped entirely inside the closure
/// (it is not `Send`).
async fn render_preview(
    doc: AsyncDocument,
    tx: UnboundedSender<Msg>,
    generation: u64,
    page: usize,
    max_w: u32,
    max_h: u32,
    file_bytes: Option<Arc<Vec<u8>>>,
) {
    let bytes = match file_bytes {
        Some(bytes) => Ok(bytes),
        None => fetch_whole_file(&doc).await.map(Arc::new),
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            tx.send(Msg::PreviewReady {
                generation,
                result: Err(error),
            })
            .ok();
            return;
        }
    };
    let render_input = Arc::clone(&bytes);
    let rendered = tokio::task::spawn_blocking(
        move || -> Result<pdfboss_render::Pixmap, String> {
            let document = pdfboss_core::Document::load(render_input.as_ref().clone())
                .map_err(|error| error.to_string())?;
            let page_object = document.page(page).map_err(|error| error.to_string())?;
            let (page_w, page_h) = page_object.size();
            let scale = fit_scale(page_w, page_h, max_w, max_h);
            pdfboss_render::render_page(&document, &page_object, scale)
                .map_err(|error| error.to_string())
        },
    )
    .await;
    let result = match rendered {
        Ok(Ok(pixmap)) => Ok(PreviewFrame {
            file_bytes: bytes,
            pixmap,
        }),
        Ok(Err(error)) => Err(error),
        Err(join_error) => Err(join_error.to_string()),
    };
    tx.send(Msg::PreviewReady { generation, result }).ok();
}

/// Fetches the entire file via one `read_span` over
/// `0..doc.file_len()` (the aio crate reports the length synchronously).
async fn fetch_whole_file(doc: &AsyncDocument) -> Result<Vec<u8>, String> {
    let end = doc.file_len();
    doc.read_span(Span { start: 0, end })
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetch_whole_file_reads_exactly_file_len_bytes() {
        let data = pdfboss_testkit::simple_doc("Hello");
        let doc = AsyncDocument::from_bytes(data.clone())
            .await
            .expect("fixture opens");
        assert_eq!(doc.file_len(), data.len() as u64, "reported length");
        let fetched = fetch_whole_file(&doc).await.expect("whole-file fetch");
        assert_eq!(fetched, data, "fetch covers the entire file");
    }
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo check -p pdfboss-tui` — expect success.
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-tui` — expect the whole crate suite green.
- [ ] Run clippy + fmt — expect clean.
- [ ] **Manual verification (deferred):** the interactive smoke test `cargo run -p pdfboss-cli -- tui tests/fixtures/shapes.pdf` requires Task 11's subcommand; perform it there.
- [ ] Commit: `git add crates/pdfboss-tui && git commit -m "feat(tui): tokio event loop with background command executor"`

---

### Task 11: CLI wiring — `pdfboss tui <file-or-url>`

**Files:**
- Modify: `crates/pdfboss-cli/Cargo.toml` (dependencies + features)
- Modify: `crates/pdfboss-cli/src/main.rs` (`Command` enum ~line 23, dispatch ~line 96, new functions after `cmd_obj` ~line 320, tests at the end)
- Test: unit tests inside `crates/pdfboss-cli/src/main.rs`; manual smoke on `tests/fixtures/shapes.pdf`

**Interfaces:**
- Consumes: `pdfboss_tui::run(doc: AsyncDocument, title: String) -> std::io::Result<()>` (Task 10); `pdfboss_aio::AsyncDocument::open(path) -> Result<Self>` and (http feature) `AsyncDocument::open_url(url) -> Result<Self>` (plan 02, spec-pinned); existing clap `Cli`/`Command` conventions in `crates/pdfboss-cli/src/main.rs`.
- Produces: the `pdfboss tui <file-or-url>` subcommand; CLI `http` cargo feature forwarding to `pdfboss-aio/http`.

**Steps:**

- [ ] Write the failing tests first: append inside the existing `mod tests` of `crates/pdfboss-cli/src/main.rs`:

```rust
    #[test]
    fn tui_subcommand_parses() {
        let cli = Cli::parse_from(["pdfboss", "tui", "in.pdf"]);
        let Command::Tui { target } = cli.command else {
            panic!("expected tui command");
        };
        assert_eq!(target, "in.pdf");
    }

    #[test]
    fn url_detection() {
        assert!(is_url("https://example.com/a.pdf"));
        assert!(is_url("http://example.com/a.pdf"));
        assert!(!is_url("plain.pdf"));
        assert!(!is_url("dir/httpish.pdf"));
    }

    #[test]
    fn display_title_takes_last_segment() {
        assert_eq!(display_title("dir/sub/file.pdf"), "file.pdf");
        assert_eq!(display_title("file.pdf"), "file.pdf");
        assert_eq!(
            display_title("https://example.com/docs/spec.pdf"),
            "spec.pdf"
        );
        assert_eq!(display_title("trailing/"), "trailing/");
    }
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli tui` — expect compile errors (no `Tui` variant, no `is_url`/`display_title`).
- [ ] Add the dependencies and feature to `crates/pdfboss-cli/Cargo.toml` — full new `[dependencies]` and `[features]` sections:

```toml
[dependencies]
pdfboss-core = { path = "../pdfboss-core" }
pdfboss-text = { path = "../pdfboss-text" }
pdfboss-render = { path = "../pdfboss-render" }
pdfboss-aio = { path = "../pdfboss-aio" }
pdfboss-tui = { path = "../pdfboss-tui" }
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["rt"] }

[features]
# Passthrough so `cargo build -p pdfboss-cli --features substitute-fonts`
# enables the bundled substitute faces, without the caller needing to know
# the non-obvious `--features pdfboss-render/substitute-fonts` spelling.
substitute-fonts = ["pdfboss-render/substitute-fonts"]
# Enables http(s) targets for `pdfboss tui` via the aio HTTP backend.
http = ["pdfboss-aio/http"]
```

- [ ] Add the subcommand variant to the `Command` enum in `crates/pdfboss-cli/src/main.rs` (after the `Obj` variant):

```rust
    /// Explore a PDF interactively in the terminal.
    Tui {
        /// Path to the PDF file, or an http(s) URL (requires a build with
        /// the `http` feature).
        target: String,
    },
```

- [ ] Add the dispatch arm in `main()` (after the `Command::Obj` arm):

```rust
        Command::Tui { target } => cmd_tui(&target),
```

- [ ] Add the implementation after `cmd_obj` (before `page_index`):

```rust
/// `pdfboss tui`: interactive explorer over a local file or an http(s)
/// URL, on a current-thread tokio runtime (rasterization uses the
/// runtime's blocking pool; the loop itself is single-threaded).
fn cmd_tui(target: &str) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let doc = open_async_document(target).await?;
        pdfboss_tui::run(doc, display_title(target))
            .await
            .map_err(|e| e.to_string())
    })
}

/// True for http(s) URLs; everything else is treated as a path.
fn is_url(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://")
}

/// Builds the async document: HTTP backend for URLs (behind the `http`
/// feature), file backend otherwise.
async fn open_async_document(target: &str) -> Result<pdfboss_aio::AsyncDocument, String> {
    if is_url(target) {
        #[cfg(feature = "http")]
        {
            return pdfboss_aio::AsyncDocument::open_url(target)
                .await
                .map_err(|e| e.to_string());
        }
        #[cfg(not(feature = "http"))]
        {
            return Err(
                "URL targets need pdfboss built with the `http` feature \
                 (cargo build -p pdfboss-cli --features http)"
                    .to_string(),
            );
        }
    }
    pdfboss_aio::AsyncDocument::open(target)
        .await
        .map_err(|e| e.to_string())
}

/// The status-bar title: the last path/URL segment, or the whole target.
fn display_title(target: &str) -> String {
    target
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(target)
        .to_string()
}
```

- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test -p pdfboss-cli` — expect the whole CLI suite green (existing tests untouched, three new ones pass).
- [ ] Run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings && CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check` — expect clean.
- [ ] **Manual verification:** run `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo run -p pdfboss-cli -- tui tests/fixtures/shapes.pdf` in a real terminal and walk the checklist: tree shows Document/Pages/Objects/Xref/Trailer; expanding Objects populates lazily; selecting an object fills inspector + hex; `d` on a content stream cycles pretty → raw → decoded → ops; `p` shows the shapes page as colored half-blocks with a spinner first; resizing re-renders after a beat; `/re` finds objects, `n` jumps, Backspace returns; `q` exits with the terminal restored. Repeat once with `tests/fixtures/xref-stream.pdf` and verify an objstm member's hex shows the decoded container with the member range highlighted.
- [ ] Commit: `git add crates/pdfboss-cli && git commit -m "feat(cli): pdfboss tui subcommand over file or http targets"`

---

## Final verification

- [ ] `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo test --workspace` — everything green, existing suites untouched.
- [ ] `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo clippy --workspace --all-targets -- -D warnings` — clean.
- [ ] `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo fmt --all -- --check` — clean.
- [ ] `CARGO_TARGET_DIR=$HOME/.cargo/shared-target cargo doc --workspace --no-deps` with `RUSTDOCFLAGS="-D warnings"` — clean (CI runs this).
- [ ] Grep the new crates for banned identifiers: `grep -rn "_[a-z]" crates/pdfboss-tui/src --include='*.rs' | grep -E 'let _[a-z]|fn _[a-z]|_[a-z]+:' ` — review any hit; none may be an underscore-prefixed identifier.

## Spec coverage map

| Spec bullet (pdfboss-tui + tui subcommand) | Task(s) |
| --- | --- |
| Library crate, `pub async fn run(doc, title)`, publishable, workspace-versioned, release/CI registration | 1, 10 |
| Deps: pdfboss-aio/core/render, ratatui, crossterm (event-stream), tokio, futures-util | 1 |
| Module layout app/tree/inspector/hexview/preview/search/input/ui | 2–10 |
| Tree ~35% width, Document→Pages(→Fonts/Images/Annotations/Contents)→Objects→Xref→Trailer, lazy populate on first expand | 2, 8 (layout: 8 `panes`) |
| Inspector pretty via core `pretty`; `d` cycles raw/decoded/ops one-per-line | 5, 8, 9 |
| Hex: offset│hex│ascii, byte-class colors, selection span via `read_span`, scrollable + PgUp/PgDn, objstm decoded container with member highlight | 3, 8, 9, 10 |
| Preview: `p` swaps inspector, spawn_blocking rasterize at fit-to-pane scale, `▀` half-blocks (fg=upper, bg=lower), spinner, debounced resize re-render | 6, 8, 9, 10 |
| Navigation: ↑↓/jk, ←→/hl, Tab, Enter-on-ref jump, Backspace history, g/G, q/Esc | 7, 8 |
| Search: `/` status-bar input, incremental over numbers/keys/names/strings, lazy streaming via background task + channel, n/N, Esc | 4, 7, 8, 10 |
| Event loop: `tokio::select!` over EventStream + task channels; long ops never block input | 10 |
| Errors as toasts, never panic; unusable xref still explores physically (salvage batches) | 8, 10 |
| `pdfboss tui <file-or-url>`: FileBackend / http-feature HttpBackend, current-thread runtime | 11 |
| Tests: pure-logic units (tree from Vec<Element>, search matching, hex formatting, highlight math, history stack) + 80x24 TestBackend full-frame snapshots (tree, inspector dict, hex, status bar) | 2–8 (units), 9 (snapshots) |
