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
    Page {
        index: usize,
        r: ObjRef,
    },
    FontsFolder {
        page: usize,
    },
    Font {
        r: ObjRef,
        subtype: Name,
        base_font: Option<Name>,
    },
    ImagesFolder {
        page: usize,
    },
    Image {
        r: ObjRef,
        width: u32,
        height: u32,
    },
    AnnotationsFolder {
        page: usize,
    },
    Annotation {
        r: ObjRef,
        subtype: Name,
    },
    ContentsFolder {
        page: usize,
    },
    ContentsStream {
        r: ObjRef,
    },
    ObjectsFolder,
    Object {
        r: ObjRef,
        span: Span,
        in_objstm: Option<(ObjRef, Span)>,
    },
    XrefFolder,
    XrefSection {
        kind: XrefKind,
        span: Span,
        entries: usize,
    },
    StartXref {
        offset: u64,
        span: Span,
    },
    Eof {
        span: Span,
    },
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
    /// when its data has not been requested yet, or when a prior attempt
    /// failed (and marks it Loading either way, so a re-expand after a
    /// failure retries the load instead of bricking the section).
    pub fn expand(&mut self, id: NodeId) -> Option<TreeReq> {
        if !self.is_branch(id) {
            return None;
        }
        self.nodes[id].expanded = true;
        let kind = self.nodes[id].kind.clone();
        match kind {
            NodeKind::PagesFolder
                if matches!(self.logical, LoadState::NotLoaded | LoadState::Failed) =>
            {
                self.logical = LoadState::Loading;
                Some(TreeReq::Logical)
            }
            NodeKind::ObjectsFolder | NodeKind::XrefFolder
                if matches!(self.physical, LoadState::NotLoaded | LoadState::Failed) =>
            {
                self.physical = LoadState::Loading;
                Some(TreeReq::Physical)
            }
            NodeKind::ContentsFolder { page }
                if matches!(
                    self.nodes[id].load,
                    LoadState::NotLoaded | LoadState::Failed
                ) =>
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
    ///
    /// Note: this unconditionally retargets `self.selected` to `id`'s parent
    /// on the leaf/collapsed path, regardless of what was selected before the
    /// call. Callers driving this from a "collapse the current selection" key
    /// binding should pass the *currently selected* node; passing an
    /// unrelated ancestor teleports selection there instead. Separately, if a
    /// caller collapses a branch that is an ancestor of `self.selected`
    /// without moving the selection first, the selected node becomes hidden
    /// and `selected_position`'s `unwrap_or(0)` fallback silently treats it
    /// as row 0 on the next `select_next`/`select_prev` — callers must
    /// re-clamp `selected` to a currently visible row after any collapse.
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
    ///
    /// Idempotent per section: a batch delivered again after its section
    /// already finished loading (e.g. a duplicate delivery from the
    /// background runner) is a no-op rather than re-adding every node.
    pub fn apply_batch(&mut self, req: TreeReq, elements: &[Element], done: bool) {
        match req {
            TreeReq::Physical if self.physical == LoadState::Loaded => return,
            TreeReq::Logical if self.logical == LoadState::Loaded => return,
            TreeReq::Physical | TreeReq::Logical | TreeReq::Contents { .. } => {}
        }
        for element in elements {
            match element {
                Element::Header { version, span } => {
                    self.version = *version;
                    self.header_span = Some(*span);
                }
                // The parsed object value is not retained (`..`): the
                // inspector re-fetches on selection, keeping the tree small.
                Element::IndirectObject {
                    r, span, in_objstm, ..
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
                Element::XrefSection {
                    kind,
                    span,
                    entries,
                } => {
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
                        NodeKind::Page {
                            index: *index,
                            r: *r,
                        },
                    );
                    let fonts = self.add(Some(page_node), NodeKind::FontsFolder { page: *index });
                    let images = self.add(Some(page_node), NodeKind::ImagesFolder { page: *index });
                    let annotations = self.add(
                        Some(page_node),
                        NodeKind::AnnotationsFolder { page: *index },
                    );
                    let contents =
                        self.add(Some(page_node), NodeKind::ContentsFolder { page: *index });
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
                // Font/Image/Annotation all rely on the producer guarantee
                // that a page's `Element::Page` arrives before its
                // fonts/images/annotations (core's `page_elements` emits the
                // page first, in document order). An element whose page
                // hasn't been seen yet finds no entry in `page_folders` and
                // is silently dropped rather than queued.
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
                        .flat_map(|folders| [folders.fonts, folders.images, folders.annotations])
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
    ///
    /// Idempotent: a page whose Contents folder is already `Loaded` is left
    /// untouched, so a duplicate delivery does not duplicate stream nodes.
    pub fn apply_contents(&mut self, page: usize, refs: &[ObjRef]) {
        let Some(folders) = self.page_folders.get(&page).copied() else {
            return;
        };
        if self.nodes[folders.contents].load == LoadState::Loaded {
            return;
        }
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
            NodeKind::FontsFolder { .. } => self.folder_label("Fonts", id),
            NodeKind::Font {
                r,
                subtype,
                base_font,
            } => {
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
                LoadState::NotLoaded | LoadState::Loading | LoadState::Failed => "Xref".to_string(),
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
            LoadState::NotLoaded | LoadState::Loading | LoadState::Failed => name.to_string(),
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

    /// The `pdfboss q` expression addressing node `id` in the wire tree.
    /// Nodes carrying an object ref address `.objects["N G"]` (the wire
    /// entries under `.pages[]` are summaries, not the objects); `%%EOF`
    /// has no wire form at all.
    pub fn query(&self, id: NodeId) -> Option<String> {
        match self.nodes[id].kind {
            NodeKind::Document => Some(".".to_string()),
            NodeKind::PagesFolder => Some(".pages".to_string()),
            NodeKind::Page { index, .. } => Some(format!(".pages[{index}]")),
            NodeKind::FontsFolder { page } => Some(format!(".pages[{page}].fonts")),
            NodeKind::ImagesFolder { page } => Some(format!(".pages[{page}].images")),
            NodeKind::AnnotationsFolder { page } => Some(format!(".pages[{page}].annotations")),
            NodeKind::ContentsFolder { page } => {
                let r = self.page_ref(page)?;
                Some(format!(".objects[\"{} {}\"].value.Contents", r.num, r.gen))
            }
            NodeKind::ObjectsFolder => Some(".objects".to_string()),
            NodeKind::Object { r, .. }
            | NodeKind::Font { r, .. }
            | NodeKind::Image { r, .. }
            | NodeKind::Annotation { r, .. }
            | NodeKind::ContentsStream { r } => Some(format!(".objects[\"{} {}\"]", r.num, r.gen)),
            NodeKind::XrefFolder => Some(".xref".to_string()),
            NodeKind::XrefSection { .. } => {
                let index = self.nodes[self.xref_folder]
                    .children
                    .iter()
                    .filter(|child| {
                        matches!(self.nodes[**child].kind, NodeKind::XrefSection { .. })
                    })
                    .position(|child| *child == id)?;
                Some(format!(".xref[{index}]"))
            }
            NodeKind::StartXref { .. } => Some(".startxref".to_string()),
            NodeKind::Trailer => Some(".trailer".to_string()),
            NodeKind::Eof { .. } => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::elements::{Element, Span, XrefKind};
    use pdfboss_core::{Dict, Name, ObjRef, Object};

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
                span: Span {
                    start: 64,
                    end: 120,
                },
                in_objstm: Some((obj_ref(9), Span { start: 4, end: 30 })),
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
                dict: Dict::new(),
                span: Span {
                    start: 260,
                    end: 300,
                },
            },
            Element::StartXref {
                offset: 120,
                span: Span {
                    start: 300,
                    end: 314,
                },
            },
            Element::Eof {
                span: Span {
                    start: 314,
                    end: 320,
                },
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
        assert_eq!(
            tree.trailer_span,
            Some(Span {
                start: 260,
                end: 300
            })
        );
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
                Span {
                    start: 64,
                    end: 120
                },
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
    fn query_addresses_every_node_kind_in_the_wire_tree() {
        let mut tree = TreeState::new((1, 7), 1);
        tree.apply_batch(TreeReq::Physical, &physical_batch(), true);
        tree.apply_batch(TreeReq::Logical, &logical_batch(), true);
        tree.apply_contents(0, &[obj_ref(4)]);

        assert_eq!(tree.query(tree.root).as_deref(), Some("."));
        assert_eq!(tree.query(tree.pages_folder).as_deref(), Some(".pages"));
        assert_eq!(tree.query(tree.objects_folder).as_deref(), Some(".objects"));
        assert_eq!(tree.query(tree.xref_folder).as_deref(), Some(".xref"));
        assert_eq!(tree.query(tree.trailer_node).as_deref(), Some(".trailer"));

        let objects = tree.node(tree.objects_folder).children.clone();
        assert_eq!(tree.query(objects[0]).as_deref(), Some(".objects[\"1 0\"]"));
        assert_eq!(tree.query(objects[1]).as_deref(), Some(".objects[\"2 0\"]"));

        let xref_children = tree.node(tree.xref_folder).children.clone();
        assert_eq!(tree.query(xref_children[0]).as_deref(), Some(".xref[0]"));
        assert_eq!(tree.query(xref_children[1]).as_deref(), Some(".startxref"));
        assert_eq!(tree.query(xref_children[2]), None, "%%EOF has no wire form");

        let page = tree.node(tree.pages_folder).children[0];
        assert_eq!(tree.query(page).as_deref(), Some(".pages[0]"));
        for folder in tree.node(page).children.clone() {
            let expected = match tree.node(folder).kind {
                NodeKind::FontsFolder { .. } => ".pages[0].fonts",
                NodeKind::ImagesFolder { .. } => ".pages[0].images",
                NodeKind::AnnotationsFolder { .. } => ".pages[0].annotations",
                // The wire tree has no contents array; the page object's
                // /Contents entry is the addressable form.
                NodeKind::ContentsFolder { .. } => ".objects[\"3 0\"].value.Contents",
                ref other => panic!("unexpected page child {other:?}"),
            };
            assert_eq!(tree.query(folder).as_deref(), Some(expected));
            for leaf in tree.node(folder).children.clone() {
                let expected = match tree.node(leaf).kind {
                    NodeKind::Font { .. } => ".objects[\"5 0\"]",
                    NodeKind::Image { .. } => ".objects[\"7 0\"]",
                    NodeKind::Annotation { .. } => ".objects[\"8 0\"]",
                    NodeKind::ContentsStream { .. } => ".objects[\"4 0\"]",
                    ref other => panic!("unexpected leaf {other:?}"),
                };
                assert_eq!(tree.query(leaf).as_deref(), Some(expected));
            }
        }
    }

    /// `.xref[i]` must index xref *sections* in element-stream order, the
    /// exact order `build_tree` pushes them into the wire array, skipping
    /// the startxref/%%EOF siblings interleaved in the tree.
    #[test]
    fn xref_query_index_counts_sections_only() {
        let elements = vec![
            Element::XrefSection {
                kind: XrefKind::Table,
                span: Span { start: 0, end: 10 },
                entries: 1,
            },
            Element::StartXref {
                offset: 0,
                span: Span { start: 10, end: 20 },
            },
            Element::Eof {
                span: Span { start: 20, end: 26 },
            },
            Element::XrefSection {
                kind: XrefKind::Stream,
                span: Span { start: 26, end: 40 },
                entries: 2,
            },
        ];
        let mut tree = TreeState::new((1, 7), 0);
        tree.apply_batch(TreeReq::Physical, &elements, true);
        let children = tree.node(tree.xref_folder).children.clone();
        assert_eq!(tree.query(children[0]).as_deref(), Some(".xref[0]"));
        assert_eq!(tree.query(children[3]).as_deref(), Some(".xref[1]"));
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
        assert!(
            tree.collapse_or_parent(first_object),
            "leaf climbs to parent"
        );
        assert_eq!(tree.selected, tree.objects_folder);
        assert!(
            tree.collapse_or_parent(tree.objects_folder),
            "folds open branch"
        );
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
        assert!(tree.visible_rows().iter().any(|row| row.id == id));
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

    #[test]
    fn reapplying_a_batch_is_a_no_op() {
        let mut tree = TreeState::new((1, 7), 1);
        tree.apply_batch(TreeReq::Physical, &physical_batch(), true);
        tree.apply_batch(TreeReq::Logical, &logical_batch(), true);
        tree.expand(tree.objects_folder);
        tree.expand(tree.xref_folder);
        tree.expand(tree.pages_folder);
        let page_id = tree.node(tree.pages_folder).children[0];
        tree.expand(page_id);
        let before = tree.visible_rows();

        tree.apply_batch(TreeReq::Physical, &physical_batch(), true);
        tree.apply_batch(TreeReq::Logical, &logical_batch(), true);
        let after = tree.visible_rows();

        assert_eq!(
            before, after,
            "reapplying an already-loaded batch must not duplicate nodes"
        );
    }

    #[test]
    fn failed_section_can_be_retried_by_expanding() {
        let mut tree = TreeState::new((1, 7), 1);
        assert_eq!(tree.expand(tree.objects_folder), Some(TreeReq::Physical));
        tree.mark_failed(TreeReq::Physical);
        assert_eq!(tree.physical, LoadState::Failed);
        assert_eq!(
            tree.expand(tree.objects_folder),
            Some(TreeReq::Physical),
            "re-expanding a failed section must re-request its load"
        );
        assert_eq!(tree.physical, LoadState::Loading);
    }
}
