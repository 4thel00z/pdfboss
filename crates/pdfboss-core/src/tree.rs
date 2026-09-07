//! Name trees and number trees (ISO 32000-1 §7.9.6 and §7.9.7): sorted
//! key-value maps spread over `/Kids` nodes, each labelled with the
//! `/Limits` its keys fall in, so one key can be found without reading the
//! whole tree, or every pair listed leaf by leaf.

use crate::hash::FastSet;
use crate::object::{Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;

/// Maximum nodes visited in one walk: past it the walk gives up, so a
/// `/Kids` graph that a visited set cannot catch (direct dictionaries
/// repeated by value) still cannot spin.
pub const MAX_TREE_NODES: usize = 4096;

/// A tree's key type: how a leaf pair's key or a `/Limits` bound reads
/// from its object, and which node entry holds the leaf pairs.
pub trait TreeKey: Ord + Sized {
    /// The leaf entry: `Names` in a name tree, `Nums` in a number tree.
    const PAIRS: &'static str;

    /// Reads a key from a resolved object; `None` for the wrong type.
    fn from_object(o: &Object) -> Option<Self>;
}

/// Name tree keys are strings compared byte by byte, shorter keys before
/// longer ones with the same prefix, whatever their encoding; `Vec<u8>`'s
/// ordering is exactly that.
///
/// Covers ISO 32000-1 §7.9.6.
impl TreeKey for Vec<u8> {
    const PAIRS: &'static str = "Names";

    fn from_object(o: &Object) -> Option<Self> {
        o.as_str_bytes().map(<[u8]>::to_vec)
    }
}

/// Number tree keys are integers in numerical order.
///
/// Covers ISO 32000-1 §7.9.7.
impl TreeKey for i64 {
    const PAIRS: &'static str = "Nums";

    fn from_object(o: &Object) -> Option<Self> {
        o.as_int()
    }
}

/// Looks `key` up in the tree rooted at `root`: the root's own pairs
/// first, then the `/Kids` whose `/Limits` admit the key, depth first. The
/// value comes back unresolved, so a stream stays an indirect reference.
/// Malformed nodes are skipped, a node without readable limits is searched
/// regardless, a kid already visited is not entered again, and the walk
/// stops after [`MAX_TREE_NODES`] nodes.
///
/// Covers ISO 32000-1 §7.9.6 and §7.9.7.
pub async fn lookup<K: TreeKey, S: AsyncObjectSource>(
    src: &S,
    root: &Dict,
    key: &K,
) -> Option<Object> {
    let mut walk = Walk::default();
    let mut pending: Vec<Dict> = vec![root.clone()];
    while let Some(node) = pending.pop() {
        if !walk.enter() {
            return None;
        }
        for (k, value) in pairs::<K, S>(src, &node).await {
            if k == *key {
                return Some(value);
            }
        }
        for kid in kids(src, &node).await.iter().rev() {
            let Some(kid) = walk.first_visit(src, kid).await else {
                continue;
            };
            if within_limits(src, &kid, key).await {
                pending.push(kid);
            }
        }
    }
    None
}

/// Every key-value pair in the tree rooted at `root`, in tree order: a
/// node's own pairs, then each kid's in turn. Values come back unresolved.
/// Malformed nodes are skipped, a kid already visited is not entered again,
/// and the walk stops after [`MAX_TREE_NODES`] nodes.
///
/// Covers ISO 32000-1 §7.9.6 and §7.9.7.
pub async fn entries<K: TreeKey, S: AsyncObjectSource>(src: &S, root: &Dict) -> Vec<(K, Object)> {
    let mut walk = Walk::default();
    let mut found = Vec::new();
    let mut pending: Vec<Dict> = vec![root.clone()];
    while let Some(node) = pending.pop() {
        if !walk.enter() {
            break;
        }
        found.extend(pairs::<K, S>(src, &node).await);
        for kid in kids(src, &node).await.iter().rev() {
            if let Some(kid) = walk.first_visit(src, kid).await {
                pending.push(kid);
            }
        }
    }
    found
}

/// The bookkeeping of one walk: the kids entered so far, by reference, and
/// the node budget.
#[derive(Default)]
struct Walk {
    visited: FastSet<ObjRef>,
    entered: usize,
}

impl Walk {
    /// Counts a node against the budget; `false` once it is spent.
    fn enter(&mut self) -> bool {
        self.entered += 1;
        self.entered <= MAX_TREE_NODES
    }

    /// The kid as a dictionary the first time its reference is seen; `None`
    /// for a repeat, or for a kid that is not a dictionary.
    async fn first_visit<S: AsyncObjectSource>(&mut self, src: &S, kid: &Object) -> Option<Dict> {
        if let Some(r) = kid.as_ref() {
            if !self.visited.insert(r) {
                return None;
            }
        }
        resolved_dict(src, kid).await
    }
}

/// A node's `/Kids`, unresolved; empty when absent or not an array.
async fn kids<S: AsyncObjectSource>(src: &S, node: &Dict) -> Vec<Object> {
    let Some(kids) = node.get("Kids") else {
        return Vec::new();
    };
    match src.resolve(kids).await {
        Ok(Object::Array(kids)) => kids,
        _ => Vec::new(),
    }
}

/// A node's leaf pairs (`/Names` or `/Nums`) whose keys read as `K`,
/// values unresolved; a pair whose key does not is skipped.
async fn pairs<K: TreeKey, S: AsyncObjectSource>(src: &S, node: &Dict) -> Vec<(K, Object)> {
    let Some(pairs) = node.get(K::PAIRS) else {
        return Vec::new();
    };
    let Ok(Object::Array(pairs)) = src.resolve(pairs).await else {
        return Vec::new();
    };
    let mut found = Vec::with_capacity(pairs.len() / 2);
    for [key, value] in pairs.as_chunks::<2>().0 {
        if let Some(key) = key_of(src, key).await {
            found.push((key, value.clone()));
        }
    }
    found
}

/// Whether `key` falls in a node's `/Limits`; a node without readable
/// limits is searched regardless.
///
/// Covers ISO 32000-1 §7.9.6 and §7.9.7.
async fn within_limits<K: TreeKey, S: AsyncObjectSource>(src: &S, node: &Dict, key: &K) -> bool {
    let Some(limits) = node.get("Limits") else {
        return true;
    };
    let Ok(Object::Array(limits)) = src.resolve(limits).await else {
        return true;
    };
    let (Some(lo), Some(hi)) = (limits.first(), limits.get(1)) else {
        return true;
    };
    match (key_of::<K, S>(src, lo).await, key_of::<K, S>(src, hi).await) {
        (Some(lo), Some(hi)) => lo <= *key && *key <= hi,
        _ => true,
    }
}

/// A key or limit read from its object, resolving an indirect one.
async fn key_of<K: TreeKey, S: AsyncObjectSource>(src: &S, o: &Object) -> Option<K> {
    if let Some(key) = K::from_object(o) {
        return Some(key);
    }
    K::from_object(&src.resolve(o).await.ok()?)
}

/// Resolves `o` to a dictionary, a stream's dictionary included.
pub(crate) async fn resolved_dict<S: AsyncObjectSource>(src: &S, o: &Object) -> Option<Dict> {
    match src.resolve(o).await.ok()? {
        Object::Dict(dict) => Some(dict),
        Object::Stream(stream) => Some(stream.dict),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{block_on, Document, Immediate};
    use pdfboss_testkit::PdfBuilder;

    /// A document whose object 10 is a tree root; `objects` supplies the
    /// tree (10 and up).
    fn tree_doc(objects: &[(u32, &str)]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        for (num, body) in objects {
            b.object(*num, body);
        }
        Document::load(b.build(1)).expect("load")
    }

    fn root(doc: &Document) -> Dict {
        doc.get(ObjRef { num: 10, gen: 0 })
            .unwrap()
            .as_dict()
            .cloned()
            .unwrap()
    }

    fn find(doc: &Document, key: &str) -> Option<Object> {
        block_on(lookup(
            &Immediate(doc),
            &root(doc),
            &key.as_bytes().to_vec(),
        ))
    }

    fn keys(doc: &Document) -> Vec<String> {
        block_on(entries::<Vec<u8>, _>(&Immediate(doc), &root(doc)))
            .into_iter()
            .map(|(k, _)| String::from_utf8(k).unwrap())
            .collect()
    }

    /// The clause's own example, abbreviated: a root of three intermediate
    /// nodes, each with leaf nodes, values as indirect integers.
    fn elements() -> Document {
        tree_doc(&[
            (10, "<< /Kids [11 0 R 12 0 R 13 0 R] >>"),
            (
                11,
                "<< /Limits [(Actinium) (Gold)] /Kids [14 0 R 15 0 R] >>",
            ),
            (
                12,
                "<< /Limits [(Hafnium) (Protactinium)] /Kids [16 0 R] >>",
            ),
            (
                13,
                "<< /Limits [(Radium) (Zirconium)] /Names [(Radium) 88 (Zirconium) 40] >>",
            ),
            (
                14,
                "<< /Limits [(Actinium) (Astatine)] /Names [(Actinium) 20 0 R (Astatine) 85] >>",
            ),
            (
                15,
                "<< /Limits [(Gadolinium) (Gold)] /Names [(Gadolinium) 64 (Gold) 21 0 R] >>",
            ),
            (
                16,
                "<< /Limits [(Hafnium) (Hydrogen)] /Names [(Hafnium) 72 (Hydrogen) 1] >>",
            ),
            (20, "89"),
            (21, "79"),
        ])
    }

    // Covers ISO 32000-1 §7.9.6.
    #[test]
    fn name_lookup_descends_kids_by_limits() {
        let doc = elements();
        assert_eq!(find(&doc, "Hydrogen"), Some(Object::Int(1)));
        assert_eq!(find(&doc, "Radium"), Some(Object::Int(88)));
        // Values come back as stored: an indirect value stays a reference.
        assert_eq!(
            find(&doc, "Gold"),
            Some(Object::Ref(ObjRef { num: 21, gen: 0 }))
        );
        assert_eq!(find(&doc, "Gallium"), None, "inside a range, absent");
        assert_eq!(find(&doc, "Zzz"), None, "past every limit");
        assert_eq!(find(&doc, ""), None, "before every limit");
    }

    // Covers ISO 32000-1 §7.9.6.
    #[test]
    fn name_keys_compare_bytewise_shorter_first() {
        // Two leaves whose limits only make sense byte by byte: (a) sorts
        // before (ab), and the UTF-16 key with its byte order mark (0xFE
        // first) sorts after every ASCII key.
        let doc = tree_doc(&[
            (10, "<< /Kids [11 0 R 12 0 R] >>"),
            (11, "<< /Limits [(a) (a)] /Names [(a) 2] >>"),
            (
                12,
                "<< /Limits [(ab) <FEFF00E9>] /Names [(ab) 3 (b) 4 <FEFF00E9> 1] >>",
            ),
        ]);
        assert_eq!(find(&doc, "a"), Some(Object::Int(2)));
        assert_eq!(find(&doc, "ab"), Some(Object::Int(3)));
        assert_eq!(find(&doc, "b"), Some(Object::Int(4)));
        let bom = block_on(lookup(
            &Immediate(&doc),
            &root(&doc),
            &b"\xFE\xFF\x00\xE9".to_vec(),
        ));
        assert_eq!(bom, Some(Object::Int(1)));
        assert_eq!(find(&doc, "aa"), None, "between (a) and (ab), absent");
    }

    // Covers ISO 32000-1 §7.9.6.
    #[test]
    fn entries_list_every_leaf_in_order() {
        let doc = elements();
        assert_eq!(
            keys(&doc),
            [
                "Actinium",
                "Astatine",
                "Gadolinium",
                "Gold",
                "Hafnium",
                "Hydrogen",
                "Radium",
                "Zirconium"
            ]
        );
        // A root that is its own single leaf lists its pairs too.
        let flat = tree_doc(&[(10, "<< /Names [(x) 1 (y) 2] >>")]);
        assert_eq!(keys(&flat), ["x", "y"]);
    }

    // Covers ISO 32000-1 §7.9.6.
    #[test]
    fn malformed_nodes_and_cycles_end_the_walk() {
        // A kid that refers back to the root, a kid that is not a
        // dictionary, a pair whose key is not a string and a leaf without
        // limits (searched regardless).
        let doc = tree_doc(&[
            (10, "<< /Kids [10 0 R 7 11 0 R 12 0 R] >>"),
            (
                11,
                "<< /Limits [(a) (b)] /Names [(a) 1 /NotAKey 9 (b) 2] >>",
            ),
            (12, "<< /Names [(c) 3] >>"),
        ]);
        assert_eq!(find(&doc, "c"), Some(Object::Int(3)));
        assert_eq!(find(&doc, "b"), Some(Object::Int(2)));
        assert_eq!(find(&doc, "zzz"), None);
        assert_eq!(keys(&doc), ["a", "b", "c"], "each leaf once, in order");
    }

    // Covers ISO 32000-1 §7.9.7.
    #[test]
    fn number_lookup_reads_nums_leaves() {
        let doc = tree_doc(&[
            (10, "<< /Kids [11 0 R 12 0 R] >>"),
            (11, "<< /Limits [0 9] /Nums [0 /Zero 9 /Nine] >>"),
            (12, "<< /Limits [10 19] /Nums [10 /Ten] >>"),
        ]);
        let ten = block_on(lookup(&Immediate(&doc), &root(&doc), &10i64));
        assert_eq!(ten, Some(Object::Name(crate::object::Name("Ten".into()))));
        let all: Vec<i64> = block_on(entries::<i64, _>(&Immediate(&doc), &root(&doc)))
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(all, [0, 9, 10]);
    }
}
