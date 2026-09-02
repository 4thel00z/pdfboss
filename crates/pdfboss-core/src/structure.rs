//! Structure-tree reading order (ISO 32000-1 §14.7): where a page's
//! marked-content sequences sit in the document's logical structure, so a
//! tagged page can be read in the order its author declared.

use std::sync::Arc;

use crate::document::Page;
use crate::hash::{FastMap, FastSet};
use crate::object::{Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;

/// Maximum number of ancestors walked from an element up to the root.
/// Deeper ancestry reads as malformed and leaves the element unranked.
const MAX_ELEMENT_DEPTH: usize = 64;

/// Maximum parent-tree nodes visited for one lookup: past it the lookup
/// gives up, so a cyclic `/Kids` graph cannot spin the walk.
const MAX_NUMBER_TREE_NODES: usize = 4096;

/// One marked-content sequence: its `/MCID`, and the `/StructParents` key of
/// the content stream it appeared in: the page's, or a form XObject's own
/// when the form declares one (§14.7.4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MarkedContentId {
    pub parents: u32,
    pub mcid: u32,
}

/// The document's structure tree root (`/StructTreeRoot`), loaded once per
/// document and asked per page where that page's marked content sits in
/// the tree. `/MarkInfo` is never consulted: a tree with leaves counts,
/// whatever the file says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureTree {
    root: Dict,
    root_ref: Option<ObjRef>,
}

impl StructureTree {
    /// Loads the catalog's `/StructTreeRoot`, or `None` when the document
    /// declares none, or when the entry is missing or unreadable, which
    /// leaves every page in content order.
    pub async fn load_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Option<StructureTree> {
        let root = trailer.get("Root")?;
        let catalog = src.resolve(root).await.ok()?;
        let entry = catalog.as_dict()?.get("StructTreeRoot")?;
        let root_ref = entry.as_ref();
        let resolved = src.resolve(entry).await.ok()?;
        let root = resolved.as_dict()?.clone();
        Some(StructureTree { root, root_ref })
    }

    /// Ranks `ids`, one page's marked-content sequences, by their position in
    /// the tree's depth-first order: 0 for the first the tree reaches, and so
    /// on. An id the tree never reaches (untagged content, a key the parent
    /// tree lacks, an element whose ancestry is broken) is absent, so an
    /// empty map means the page has no leaves in the tree.
    ///
    /// The lookup goes through the parent tree (`/ParentTree`, keyed by
    /// `/StructParents`) and each element's `/P` chain, so it costs the page's
    /// own elements, never a walk of the whole tree.
    pub async fn ranks_with<S: AsyncObjectSource>(
        &self,
        src: &S,
        page: &Page,
        ids: &[MarkedContentId],
    ) -> FastMap<MarkedContentId, u32> {
        let mut ranks: FastMap<MarkedContentId, u32> = FastMap::default();
        let Some(parent_tree) = self.root.get("ParentTree") else {
            return ranks;
        };
        let Some(parent_tree) = resolved_dict(src, parent_tree).await else {
            return ranks;
        };
        let mut walk = Walk {
            src,
            page_ref: page.object_ref(),
            root_ref: self.root_ref,
            dicts: FastMap::default(),
            paths: FastMap::default(),
            parents: FastMap::default(),
        };
        let mut keyed: Vec<(MarkedContentId, Vec<u32>)> = Vec::new();
        let mut seen: FastSet<MarkedContentId> = FastSet::default();
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            let Some(key) = walk.key_of(&parent_tree, *id).await else {
                continue;
            };
            keyed.push((*id, key));
        }
        keyed.sort_by(|a, b| a.1.cmp(&b.1));
        for (rank, (id, _)) in keyed.into_iter().enumerate() {
            ranks.insert(id, rank as u32);
        }
        ranks
    }
}

/// One page's walk through the tree: the dictionaries it has already read
/// and the paths it has already computed, so a paragraph's ancestry is
/// walked once for every marked-content sequence it contains.
struct Walk<'a, S> {
    src: &'a S,
    page_ref: Option<ObjRef>,
    root_ref: Option<ObjRef>,
    dicts: FastMap<ObjRef, Option<Arc<Dict>>>,
    /// Each element's kid-index path from the root, or `None` once its
    /// ancestry proved unwalkable.
    paths: FastMap<ObjRef, Option<Arc<Vec<u32>>>>,
    /// The parent tree's array for each `/StructParents` key seen.
    parents: FastMap<u32, Option<Arc<Vec<Object>>>>,
}

impl<S: AsyncObjectSource> Walk<'_, S> {
    /// The sort key of one marked-content sequence: its element's path from
    /// the root, then its own index among the element's kids.
    async fn key_of(&mut self, parent_tree: &Dict, id: MarkedContentId) -> Option<Vec<u32>> {
        let elements = self.parent_array(parent_tree, id.parents).await?;
        let element = elements.get(id.mcid as usize)?.as_ref()?;
        let path = self.path_of(element).await?;
        let dict = self.dict(element).await?;
        let index = self.mcid_index(&dict, id.mcid).await?;
        let mut key = Vec::with_capacity(path.len() + 1);
        key.extend_from_slice(&path);
        key.push(index);
        Some(key)
    }

    /// The parent tree's entry for a `/StructParents` key: the array whose
    /// index is a marked-content id and whose value is that id's element.
    async fn parent_array(&mut self, parent_tree: &Dict, key: u32) -> Option<Arc<Vec<Object>>> {
        if let Some(cached) = self.parents.get(&key) {
            return cached.clone();
        }
        let found = match number_tree_lookup(self.src, parent_tree, i64::from(key)).await {
            Some(entry) => match self.src.resolve(&entry).await.ok()? {
                Object::Array(items) => Some(Arc::new(items)),
                _ => None,
            },
            None => None,
        };
        self.parents.insert(key, found.clone());
        found
    }

    async fn dict(&mut self, r: ObjRef) -> Option<Arc<Dict>> {
        if let Some(cached) = self.dicts.get(&r) {
            return cached.clone();
        }
        let loaded = match self.src.get(r).await.ok()? {
            Object::Dict(dict) => Some(Arc::new(dict)),
            Object::Stream(stream) => Some(Arc::new(stream.dict)),
            _ => None,
        };
        self.dicts.insert(r, loaded.clone());
        loaded
    }

    /// An element's kid-index path from the root: the ancestry is followed
    /// up through `/P` until it reaches the structure tree root, then each
    /// ancestor's index among its parent's kids is read on the way back down.
    /// Every ancestor's own path is remembered as a by-product.
    async fn path_of(&mut self, element: ObjRef) -> Option<Arc<Vec<u32>>> {
        if let Some(cached) = self.paths.get(&element) {
            return cached.clone();
        }
        let mut chain: Vec<ObjRef> = vec![element];
        // The ancestor the climb stopped at, with its path: the root
        // itself, or an ancestor whose path an earlier climb computed.
        let mut stop: Option<(ObjRef, bool, Arc<Vec<u32>>)> = None;
        let mut current = element;
        for _ in 0..MAX_ELEMENT_DEPTH {
            let Some(dict) = self.dict(current).await else {
                break;
            };
            let Some(parent) = dict.get("P").and_then(Object::as_ref) else {
                break;
            };
            if self.is_root(parent).await {
                stop = Some((parent, true, Arc::new(Vec::new())));
                break;
            }
            if let Some(cached) = self.paths.get(&parent) {
                stop = cached.clone().map(|path| (parent, false, path));
                break;
            }
            chain.push(parent);
            current = parent;
        }
        let Some((mut parent_ref, mut parent_is_root, known)) = stop else {
            for r in chain {
                self.paths.insert(r, None);
            }
            return None;
        };
        // From the topmost unresolved ancestor down to the element itself.
        let mut path: Vec<u32> = (*known).clone();
        let mut resolved: Option<Arc<Vec<u32>>> = None;
        for child in chain.into_iter().rev() {
            let index = if parent_is_root {
                self.root_kid_index(child).await
            } else {
                match self.dict(parent_ref).await {
                    Some(parent) => kid_index(&parent, child),
                    None => None,
                }
            };
            let Some(index) = index else {
                self.paths.insert(child, None);
                return None;
            };
            path.push(index);
            let shared = Arc::new(path.clone());
            self.paths.insert(child, Some(shared.clone()));
            resolved = Some(shared);
            parent_ref = child;
            parent_is_root = false;
        }
        resolved
    }

    /// Whether `r` is the structure tree root: the reference the catalog
    /// named, or failing that a dictionary typed `/StructTreeRoot`.
    async fn is_root(&mut self, r: ObjRef) -> bool {
        if self.root_ref == Some(r) {
            return true;
        }
        let Some(dict) = self.dict(r).await else {
            return false;
        };
        dict.get_name("Type")
            .is_some_and(|n| n.0 == "StructTreeRoot")
    }

    /// The index of a top-level element among the root's `/K` kids.
    async fn root_kid_index(&mut self, child: ObjRef) -> Option<u32> {
        let root_ref = self.root_ref?;
        let root = self.dict(root_ref).await?;
        kid_index(&root, child)
    }

    /// The index among `element`'s kids of the marked-content sequence
    /// numbered `mcid` on this page: a bare integer (the element's `/Pg`
    /// page) or a marked-content reference dictionary naming the page. A
    /// direct match is taken first; only when there is none are the
    /// indirect kids read, in case the reference dictionary is one of them.
    async fn mcid_index(&mut self, element: &Dict, mcid: u32) -> Option<u32> {
        let kids = kids_of(element);
        let page_ref = self.page_ref;
        let element_page = element.get_ref("Pg");
        let direct = kids.iter().position(|kid| match kid {
            Object::Int(n) => {
                u32::try_from(*n).is_ok_and(|n| n == mcid) && on_page(element_page, page_ref)
            }
            Object::Dict(d) => is_mcr(d, mcid) && on_page(d.get_ref("Pg"), page_ref),
            _ => false,
        });
        if let Some(index) = direct {
            return u32::try_from(index).ok();
        }
        for (index, kid) in kids.iter().enumerate() {
            let Some(r) = kid.as_ref() else {
                continue;
            };
            let Some(dict) = self.dict(r).await else {
                continue;
            };
            if is_mcr(&dict, mcid) && on_page(dict.get_ref("Pg"), page_ref) {
                return u32::try_from(index).ok();
            }
        }
        None
    }
}

/// Whether a kid's `/Pg` names this page. Either side unknown reads as a
/// match: a page inlined into `/Kids` has no reference to compare, and a
/// bare integer kid under an element without `/Pg` has nothing to compare
/// against.
fn on_page(pg: Option<ObjRef>, page_ref: Option<ObjRef>) -> bool {
    match (pg, page_ref) {
        (Some(pg), Some(page)) => pg == page,
        _ => true,
    }
}

/// Whether `dict` is the marked-content reference for `mcid`.
fn is_mcr(dict: &Dict, mcid: u32) -> bool {
    dict.get_int("MCID")
        .and_then(|n| u32::try_from(n).ok())
        .is_some_and(|n| n == mcid)
}

/// An element's `/K` as a list: a single kid stands alone, an array is its
/// items, nothing is empty.
fn kids_of(element: &Dict) -> Vec<Object> {
    match element.get("K") {
        Some(Object::Array(items)) => items.clone(),
        Some(single) => vec![single.clone()],
        None => Vec::new(),
    }
}

/// The index of `child` among `parent`'s kids, by reference identity.
fn kid_index(parent: &Dict, child: ObjRef) -> Option<u32> {
    let index = kids_of(parent)
        .iter()
        .position(|kid| kid.as_ref() == Some(child))?;
    u32::try_from(index).ok()
}

/// Resolves `o` to a dictionary, a stream's dictionary included.
async fn resolved_dict<S: AsyncObjectSource>(src: &S, o: &Object) -> Option<Dict> {
    match src.resolve(o).await.ok()? {
        Object::Dict(dict) => Some(dict),
        Object::Stream(stream) => Some(stream.dict),
        _ => None,
    }
}

/// Looks `key` up in a number tree (ISO 32000-1 §7.9.7): `/Nums` holds the
/// leaf pairs, `/Kids` the subtrees, each with the `/Limits` its keys fall
/// in. The value comes back unresolved. Malformed nodes are skipped, and
/// the walk stops after [`MAX_NUMBER_TREE_NODES`] nodes.
async fn number_tree_lookup<S: AsyncObjectSource>(
    src: &S,
    root: &Dict,
    key: i64,
) -> Option<Object> {
    let mut pending: Vec<Dict> = vec![root.clone()];
    let mut visited = 0usize;
    while let Some(node) = pending.pop() {
        visited += 1;
        if visited > MAX_NUMBER_TREE_NODES {
            return None;
        }
        if let Some(nums) = node.get("Nums") {
            if let Some(found) = leaf_value(src, nums, key).await {
                return Some(found);
            }
        }
        let Some(kids) = node.get("Kids") else {
            continue;
        };
        let Ok(Object::Array(kids)) = src.resolve(kids).await else {
            continue;
        };
        for kid in kids.iter().rev() {
            let Some(kid) = resolved_dict(src, kid).await else {
                continue;
            };
            if within_limits(src, &kid, key).await {
                pending.push(kid);
            }
        }
    }
    None
}

/// Whether `key` falls in a node's `/Limits`; a node without readable
/// limits is searched regardless.
async fn within_limits<S: AsyncObjectSource>(src: &S, node: &Dict, key: i64) -> bool {
    let Some(limits) = node.get("Limits") else {
        return true;
    };
    let Ok(Object::Array(limits)) = src.resolve(limits).await else {
        return true;
    };
    match (bound(src, &limits, 0).await, bound(src, &limits, 1).await) {
        (Some(lo), Some(hi)) => lo <= key && key <= hi,
        _ => true,
    }
}

/// One `/Limits` bound as an integer, resolving an indirect one.
async fn bound<S: AsyncObjectSource>(src: &S, limits: &[Object], i: usize) -> Option<i64> {
    let o = limits.get(i)?;
    src.resolve(o).await.ok()?.as_int()
}

/// The value paired with `key` in a `/Nums` array, unresolved.
async fn leaf_value<S: AsyncObjectSource>(src: &S, nums: &Object, key: i64) -> Option<Object> {
    let Ok(Object::Array(pairs)) = src.resolve(nums).await else {
        return None;
    };
    for [number, value] in pairs.as_chunks::<2>().0 {
        let found = match number {
            Object::Int(n) => *n == key,
            other => src.resolve(other).await.ok().and_then(|v| v.as_int()) == Some(key),
        };
        if found {
            return Some(value.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{block_on, Document, Immediate};
    use pdfboss_testkit::PdfBuilder;

    fn id(parents: u32, mcid: u32) -> MarkedContentId {
        MarkedContentId { parents, mcid }
    }

    /// A one-page document whose catalog names object 10 as the structure
    /// tree root; `objects` supplies the tree (10 and up) and `page_extra`
    /// lands in the page dictionary.
    fn tagged_doc(page_extra: &str, objects: &[(u32, &str)]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] {page_extra} >>"),
        );
        for (num, body) in objects {
            b.object(*num, body);
        }
        Document::load(b.build(1)).expect("load")
    }

    /// Two paragraphs on one page, the left one holding ids 0 and 2, the
    /// right one 1 and 3: tree order is 0, 2, 1, 3.
    fn two_paragraphs(parent_tree: &str) -> Document {
        tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 14 0 R] >>",
                ),
                (12, parent_tree),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0 2] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [1 3] >>",
                ),
            ],
        )
    }

    fn ranks(doc: &Document, ids: &[MarkedContentId]) -> FastMap<MarkedContentId, u32> {
        let tree = doc.structure_tree().expect("tree");
        let page = doc.page(0).unwrap();
        block_on(tree.ranks_with(&Immediate(doc), &page, ids))
    }

    #[test]
    fn no_struct_tree_root_means_no_tree() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let doc = Document::load(b.build(1)).unwrap();
        assert!(doc.structure_tree().is_none());
    }

    #[test]
    fn ranks_follow_the_tree_not_the_ids() {
        let doc = two_paragraphs("<< /Nums [0 [13 0 R 14 0 R 13 0 R 14 0 R]] >>");
        let ranks = ranks(&doc, &[id(0, 2), id(0, 3), id(0, 0), id(0, 1)]);
        assert_eq!(ranks[&id(0, 0)], 0);
        assert_eq!(ranks[&id(0, 2)], 1);
        assert_eq!(ranks[&id(0, 1)], 2);
        assert_eq!(ranks[&id(0, 3)], 3);
    }

    #[test]
    fn parent_tree_kids_and_limits_are_descended() {
        let doc = tagged_doc(
            "/StructParents 7",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R] >>",
                ),
                (12, "<< /Kids [15 0 R 16 0 R] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
                (15, "<< /Limits [0 3] /Nums [0 [] 3 []] >>"),
                (16, "<< /Limits [7 9] /Nums [7 [13 0 R] 9 []] >>"),
            ],
        );
        let ranks = ranks(&doc, &[id(7, 0)]);
        assert_eq!(ranks[&id(7, 0)], 0);
    }

    #[test]
    fn a_page_without_a_parent_tree_entry_has_no_ranks() {
        let doc = two_paragraphs("<< /Nums [5 [13 0 R]] >>");
        assert!(ranks(&doc, &[id(0, 0), id(0, 1)]).is_empty());
    }

    #[test]
    fn untagged_and_out_of_range_ids_are_absent() {
        let doc = two_paragraphs("<< /Nums [0 [13 0 R 14 0 R 13 0 R 14 0 R]] >>");
        let ranks = ranks(&doc, &[id(0, 0), id(0, 9), id(4, 0)]);
        assert_eq!(ranks.len(), 1);
        assert_eq!(ranks[&id(0, 0)], 0);
    }

    #[test]
    fn a_broken_ancestry_leaves_only_that_element_unranked() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R] >>",
                ),
                (12, "<< /Nums [0 [13 0 R 14 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
                // Not among its parent's kids: its path cannot be read.
                (
                    14,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [1] >>",
                ),
            ],
        );
        let ranks = ranks(&doc, &[id(0, 0), id(0, 1)]);
        assert_eq!(ranks.len(), 1);
        assert_eq!(ranks[&id(0, 0)], 0);
    }

    #[test]
    fn marked_content_references_name_their_page() {
        // One element spanning two pages: the same id number on each. The
        // reference naming this page wins the index, the other is skipped.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /P /P 10 0 R \
                     /K [<< /Type /MCR /Pg 99 0 R /MCID 0 >> 15 0 R] >>",
                ),
                (12, "<< /Nums [0 [11 0 R]] >>"),
                (15, "<< /Type /MCR /Pg 3 0 R /MCID 0 >>"),
            ],
        );
        let ranks = ranks(&doc, &[id(0, 0)]);
        assert_eq!(ranks[&id(0, 0)], 0);
        let tree = doc.structure_tree().unwrap();
        let page = doc.page(0).unwrap();
        let mut walk = Walk {
            src: &Immediate(&doc),
            page_ref: page.object_ref(),
            root_ref: Some(ObjRef { num: 10, gen: 0 }),
            dicts: FastMap::default(),
            paths: FastMap::default(),
            parents: FastMap::default(),
        };
        let element = block_on(walk.dict(ObjRef { num: 11, gen: 0 })).unwrap();
        assert_eq!(block_on(walk.mcid_index(&element, 0)), Some(1));
        drop(tree);
    }

    #[test]
    fn a_bare_integer_on_another_page_is_not_this_page() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /P /P 10 0 R /Pg 99 0 R /K [0] >>",
                ),
                (12, "<< /Nums [0 [11 0 R]] >>"),
            ],
        );
        assert!(ranks(&doc, &[id(0, 0)]).is_empty());
    }

    #[test]
    fn a_form_key_ranks_alongside_the_page() {
        // The form's marked content (key 1) is a child of the second
        // paragraph; the page's (key 0) fills the first.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 14 0 R] >>",
                ),
                (12, "<< /Nums [0 [13 0 R] 1 [14 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
            ],
        );
        let ranks = ranks(&doc, &[id(1, 0), id(0, 0)]);
        assert_eq!(ranks[&id(0, 0)], 0);
        assert_eq!(ranks[&id(1, 0)], 1);
    }

    #[test]
    fn a_single_kid_needs_no_array() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K 11 0 R /ParentTree 12 0 R >>",
                ),
                (11, "<< /Type /StructElem /S /P /P 10 0 R /Pg 3 0 R /K 0 >>"),
                (12, "<< /Nums [0 [11 0 R]] >>"),
            ],
        );
        assert_eq!(ranks(&doc, &[id(0, 0)])[&id(0, 0)], 0);
    }
}
