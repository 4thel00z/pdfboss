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
use crate::markdown::MarkdownState;
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
    InspectorLoaded {
        generation: u64,
        payload: InspectorPayload,
    },
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
    /// A page's Markdown extraction finished (or failed).
    MarkdownReady {
        generation: u64,
        result: Result<String, String>,
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
    /// Advances the shared search epoch to `generation` so a still-running
    /// search task's stale-epoch check trips and it self-terminates
    /// (no task spawn).
    CancelSearch { generation: u64 },
    /// Render page `page` at fit-to-`(max_w, max_h)`-pixels scale.
    RenderPreview {
        generation: u64,
        page: usize,
        max_w: u32,
        max_h: u32,
        /// Cached whole-file bytes from an earlier render, if any.
        file_bytes: Option<Arc<Vec<u8>>>,
    },
    /// Extract page `page` as Markdown.
    ExtractMarkdown { generation: u64, page: usize },
}

/// The whole TUI state.
pub struct App {
    pub title: String,
    pub tree: TreeState,
    pub inspector: InspectorState,
    pub hex: HexState,
    pub preview: PreviewState,
    pub markdown: MarkdownState,
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
            markdown: MarkdownState::new(),
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
            "{} \u{b7} {} \u{b7} [/] search  [p] preview  [m] markdown  [q] quit",
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
                        InspectorPayload::Decoded {
                            r,
                            data,
                            passthrough,
                        } => self.inspector.set_decoded(r, data, passthrough),
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
                    } else if let Some(notice) = self.preview.notice.clone() {
                        // The page rendered, but not whole: say what was
                        // lost rather than showing a silently blank preview.
                        self.toast(format!("preview: {notice}"));
                    }
                }
                Vec::new()
            }
            Msg::MarkdownReady { generation, result } => {
                if self.markdown.apply_ready(generation, result) {
                    if let Some(error) = self.markdown.error.clone() {
                        self.toast(format!("markdown: {error}"));
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
        self.markdown.tick();
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
                vec![Cmd::CancelSearch {
                    generation: self.search.generation,
                }]
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
                    // Both render into the right-top pane.
                    self.markdown.active = false;
                    self.preview.active = true;
                    self.request_preview()
                }
            }
            Action::ToggleMarkdown => {
                if self.markdown.active {
                    self.markdown.active = false;
                    Vec::new()
                } else {
                    self.preview.active = false;
                    self.markdown.active = true;
                    self.request_markdown()
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
                Pane::Inspector if self.markdown.active => {
                    self.markdown.scroll_to(0);
                    Vec::new()
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
                Pane::Inspector if self.markdown.active => {
                    self.markdown.scroll_to(u64::MAX);
                    Vec::new()
                }
                Pane::Inspector => {
                    self.inspector.scroll = self.inspector.lines.len().saturating_sub(1) as u16;
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
            Pane::Inspector if self.markdown.active => {
                self.markdown.scroll_by(delta);
                Vec::new()
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
            Pane::Inspector if self.markdown.active => {
                self.markdown.scroll_by(direction * PAGE_JUMP as i32);
                Vec::new()
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
                        format!("version: {}.{}", self.tree.version.0, self.tree.version.1),
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
            NodeKind::XrefSection {
                kind,
                span,
                entries,
            } => {
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
                self.inspector
                    .show_message("startxref", vec![format!("offset: {offset} ({offset:#x})")]);
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
                    None if matches!(self.tree.physical, LoadState::Loaded | LoadState::Failed) => {
                        // The physical pass already ran (successfully or
                        // not) and never produced a trailer dict: showing
                        // "loading" forever would dead-end the node, since
                        // nothing will ever arrive to clear it.
                        self.inspector
                            .show_message("Trailer", vec!["no trailer found".to_string()]);
                    }
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
        match (
            self.hex.visible_window_missing(rows),
            self.hex.source.clone(),
        ) {
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

    /// Extracts the selected page as Markdown. Per page, not document-wide:
    /// the document can be HTTP-range-backed, and the pane shows one page
    /// at a time anyway.
    fn request_markdown(&mut self) -> Vec<Cmd> {
        if self.tree.page_count == 0 {
            self.toast("document has no pages to extract");
            self.markdown.active = false;
            return Vec::new();
        }
        let page = self.tree.page_of(self.tree.selected).unwrap_or(0);
        let generation = self.markdown.start_extract(page);
        vec![Cmd::ExtractMarkdown { generation, page }]
    }

    /// The pixel budget `fit_scale` fits a page into: a `C`-column,
    /// `R`-row pane offers `C x (R*2)` pixels, not `C x R` — the preview
    /// paints two vertical half-block pixels (`▀`) per terminal cell row,
    /// so the row count alone would understate the vertical budget by 2x
    /// and under-scale every rendered page.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use pdfboss_core::elements::{Element, Span, XrefKind};
    use pdfboss_core::{Dict, Name, ObjRef, Object, Stream};

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn obj_ref(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    fn physical_elements() -> Vec<Element> {
        let mut trailer = Dict::new();
        trailer.insert(Name("Root".to_string()), Object::Ref(obj_ref(1)));
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
                span: Span {
                    start: 64,
                    end: 120,
                },
                in_objstm: None,
            },
            Element::XrefSection {
                kind: XrefKind::Table,
                span: Span {
                    start: 120,
                    end: 260,
                },
                entries: 3,
            },
            Element::Trailer {
                dict: trailer,
                span: Span {
                    start: 260,
                    end: 300,
                },
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
    fn search_cancel_advances_the_epoch_past_the_in_flight_generation() {
        let mut app = loaded_app();
        app.update(key(KeyCode::Char('/')));
        let cmds = app.update(key(KeyCode::Char('a')));
        let in_flight_generation = match cmds.as_slice() {
            [Cmd::StartSearch { generation, .. }] => *generation,
            other => panic!("expected StartSearch, got {:?}", other),
        };
        let cmds = app.update(key(KeyCode::Esc));
        assert!(!app.search.active);
        match cmds.as_slice() {
            [Cmd::CancelSearch { generation }] => {
                assert!(
                    *generation > in_flight_generation,
                    "cancel's epoch must be newer than the in-flight search's generation"
                );
            }
            other => panic!("expected CancelSearch, got {:?}", other),
        }
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

    /// Controller item: a preview render in flight (its `PreviewReady`
    /// has not arrived yet) must be superseded, not raced, by a
    /// resize-driven debounce. `start_render` bumps `preview.generation`
    /// on every call, so the debounced re-render's `Cmd::RenderPreview`
    /// carries a strictly newer generation than the in-flight one, and a
    /// late reply tagged with the stale generation must be dropped by
    /// `PreviewState::apply_ready` rather than clobbering current state.
    #[test]
    fn resize_during_render_supersedes_with_new_generation() {
        let mut app = loaded_app();
        let cmds = app.update(key(KeyCode::Char('p')));
        let first_generation = match cmds.as_slice() {
            [Cmd::RenderPreview {
                generation,
                page: 0,
                ..
            }] => *generation,
            other => panic!("expected RenderPreview, got {:?}", other),
        };
        assert!(app.preview.rendering, "render is in flight");
        // Resize arrives while that render is still in flight (no
        // PreviewReady yet): the debounce must fire a *new* render whose
        // generation supersedes the in-flight one.
        app.update(Msg::Resize(100, 40));
        assert!(
            app.update(Msg::Tick).is_empty(),
            "debounce still counting down"
        );
        let cmds = app.update(Msg::Tick);
        let second_generation = match cmds.as_slice() {
            [Cmd::RenderPreview {
                generation,
                page: 0,
                ..
            }] => *generation,
            other => panic!("expected superseding RenderPreview, got {:?}", other),
        };
        assert!(
            second_generation > first_generation,
            "the resize-triggered render must bump the generation past the in-flight one"
        );
        // The stale first render finishing late must be dropped: it must
        // not clear the (now second-generation) in-flight flag or install
        // its frame.
        let cmds = app.update(Msg::PreviewReady {
            generation: first_generation,
            result: Ok(crate::preview::PreviewFrame {
                file_bytes: std::sync::Arc::new(Vec::new()),
                pixmap: pdfboss_render::Pixmap {
                    width: 1,
                    height: 1,
                    data: vec![0, 0, 0, 255],
                },
                notice: None,
            }),
        });
        assert!(cmds.is_empty());
        assert!(
            app.preview.rendering,
            "stale reply must not clear the in-flight flag for the superseding generation"
        );
        assert!(
            app.preview.pixmap.is_none(),
            "stale reply must not install its frame"
        );
    }

    /// Controller item: a render that dropped content is not an error, so
    /// it installs its (possibly blank) frame — but the status bar must say
    /// what was lost instead of leaving a blank preview unexplained.
    #[test]
    fn preview_that_dropped_content_toasts_the_summary() {
        let mut app = loaded_app();
        let cmds = app.update(key(KeyCode::Char('p')));
        let generation = match cmds.as_slice() {
            [Cmd::RenderPreview {
                generation,
                page: 0,
                ..
            }] => *generation,
            other => panic!("expected RenderPreview, got {:?}", other),
        };
        app.update(Msg::PreviewReady {
            generation,
            result: Ok(crate::preview::PreviewFrame {
                file_bytes: std::sync::Arc::new(Vec::new()),
                pixmap: pdfboss_render::Pixmap {
                    width: 1,
                    height: 1,
                    data: vec![255, 255, 255, 255],
                },
                notice: Some("1 image skipped".to_string()),
            }),
        });
        assert!(app.preview.pixmap.is_some(), "the frame still installs");
        assert!(
            app.status_line().contains("1 image skipped"),
            "status line does not mention the drop: {}",
            app.status_line()
        );
    }

    /// Controller item: the pane->pixel-budget conversion must account
    /// for the half-block 2:1 cell aspect. At 80x24 `ui::panes` gives
    /// `right_top` = 52 cols x 14 rows; the 2-cell chrome border leaves a
    /// 50 x 12 cell interior, and the vertical pixel budget doubles that
    /// (2 pixel rows per cell row) to 50 x 24 — not 50 x 12.
    #[test]
    fn preview_budget_doubles_row_height_for_half_block_aspect() {
        let mut app = loaded_app();
        let cmds = app.update(key(KeyCode::Char('p')));
        assert!(matches!(
            cmds.as_slice(),
            [Cmd::RenderPreview {
                max_w: 50,
                max_h: 24,
                ..
            }]
        ));
    }

    /// Controller item: `m` and `p` paint the same pane, so activating one
    /// must deactivate the other in both directions — otherwise the
    /// three-way draw branch would show markdown while the user believes
    /// they turned the raster preview on.
    #[test]
    fn markdown_and_preview_are_mutually_exclusive() {
        let mut app = loaded_app();
        let cmds = app.update(key(KeyCode::Char('m')));
        assert!(app.markdown.active);
        assert!(matches!(
            cmds.as_slice(),
            [Cmd::ExtractMarkdown { page: 0, .. }]
        ));
        let cmds = app.update(key(KeyCode::Char('p')));
        assert!(app.preview.active);
        assert!(!app.markdown.active, "preview replaces markdown");
        assert!(matches!(
            cmds.as_slice(),
            [Cmd::RenderPreview { page: 0, .. }]
        ));
        app.update(key(KeyCode::Char('m')));
        assert!(app.markdown.active);
        assert!(!app.preview.active, "markdown replaces preview");
        app.update(key(KeyCode::Char('m')));
        assert!(!app.markdown.active, "m toggles back off");
    }

    /// Controller item: a superseded extraction finishing late must not
    /// install its text over the newer request's.
    #[test]
    fn stale_markdown_extraction_is_dropped() {
        let mut app = loaded_app();
        let cmds = app.update(key(KeyCode::Char('m')));
        let stale = match cmds.as_slice() {
            [Cmd::ExtractMarkdown { generation, .. }] => *generation,
            other => panic!("expected ExtractMarkdown, got {:?}", other),
        };
        app.update(key(KeyCode::Char('m'))); // off
        let cmds = app.update(key(KeyCode::Char('m'))); // on again
        let current = match cmds.as_slice() {
            [Cmd::ExtractMarkdown { generation, .. }] => *generation,
            other => panic!("expected ExtractMarkdown, got {:?}", other),
        };
        assert!(current > stale);
        app.update(Msg::MarkdownReady {
            generation: stale,
            result: Ok("# stale".to_string()),
        });
        assert!(app.markdown.source.is_none(), "stale text is not installed");
        assert!(
            app.markdown.loading,
            "the newer extraction is still awaited"
        );
        app.update(Msg::MarkdownReady {
            generation: current,
            result: Ok("# fresh".to_string()),
        });
        assert_eq!(app.markdown.source.as_deref(), Some("# fresh"));
    }

    /// Controller item: the markdown pane shares `Pane::Inspector` focus,
    /// so while it is active the movement keys must scroll *it*, not move
    /// the inspector's hidden ref cursor.
    #[test]
    fn movement_scrolls_the_markdown_pane_while_it_is_active() {
        let mut app = loaded_app();
        let cmds = app.update(key(KeyCode::Char('m')));
        let generation = match cmds.as_slice() {
            [Cmd::ExtractMarkdown { generation, .. }] => *generation,
            other => panic!("expected ExtractMarkdown, got {:?}", other),
        };
        app.update(Msg::MarkdownReady {
            generation,
            result: Ok((0..40)
                .map(|n| format!("line {n}"))
                .collect::<Vec<String>>()
                .join("\n")),
        });
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Inspector);
        app.update(key(KeyCode::Char('j')));
        assert_eq!(app.markdown.scroll, 1);
        app.update(key(KeyCode::PageDown));
        assert_eq!(app.markdown.scroll, 11);
        app.update(key(KeyCode::Char('G')));
        assert_eq!(app.markdown.scroll, 39, "last of the 40 lines");
        app.update(key(KeyCode::Char('g')));
        assert_eq!(app.markdown.scroll, 0);
        assert!(
            app.inspector.ref_cursor.is_none(),
            "the hidden inspector must not have moved"
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
            "t.pdf \u{b7} /Document \u{b7} [/] search  [p] preview  [m] markdown  [q] quit"
        );
        app.update(key(KeyCode::Char('/')));
        app.update(key(KeyCode::Char('a')));
        assert_eq!(app.status_line(), "/a \u{b7} 0 hits \u{2026}");
    }

    #[test]
    fn trailer_node_shows_message_when_physical_pass_found_no_trailer() {
        let mut app = App::new("t.pdf".to_string(), (1, 7), 1, (80, 24));
        // Physical pass completes without ever emitting `Element::Trailer`
        // (e.g. a damaged trailer region): `trailer_dict` stays `None`
        // even though the pass is done.
        let elements = vec![Element::IndirectObject {
            r: obj_ref(1),
            object: Object::Null,
            span: Span { start: 15, end: 64 },
            in_objstm: None,
        }];
        let cmds = app.update(Msg::TreeBatch {
            req: crate::tree::TreeReq::Physical,
            elements,
            errors: 0,
            done: true,
        });
        assert!(cmds.is_empty(), "root selection refresh needs no fetches");
        assert_eq!(app.tree.physical, crate::tree::LoadState::Loaded);
        assert!(app.tree.trailer_dict.is_none());

        let cmds = app.update(key(KeyCode::Char('G'))); // select bottom: Trailer
        assert_eq!(app.tree.selected, app.tree.trailer_node);
        assert!(
            cmds.is_empty(),
            "physical already loaded and still no trailer needs no fetch"
        );
        assert!(
            !app.inspector.loading,
            "must not dead-end on a perpetual loading state"
        );
        assert_eq!(app.inspector.title, "Trailer");
        assert!(
            app.inspector
                .lines
                .iter()
                .any(|line| line.contains("no trailer")),
            "expected a no-trailer message, got {:?}",
            app.inspector.lines
        );
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
