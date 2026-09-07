//! The document's name dictionary (ISO 32000-1 §7.7.4): the catalog's
//! `/Names` entry, whose entries are the roots of name trees, one per
//! category of object that can be referred to by name.

use crate::object::{Dict, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{self, resolved_dict};

/// The name trees of Table 31, named by their key in the name dictionary.
///
/// Covers ISO 32000-1 §7.7.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameTree {
    /// `/Dests`: name strings to destinations (§12.3.2.3).
    Dests,
    /// `/AP`: name strings to annotation appearance streams (§12.5.5).
    Ap,
    /// `/JavaScript`: name strings to document-level JavaScript actions.
    JavaScript,
    /// `/Pages`: name strings to visible pages used as templates.
    Pages,
    /// `/Templates`: name strings to invisible template pages.
    Templates,
    /// `/IDS`: digital identifiers to Web Capture content sets.
    Ids,
    /// `/URLS`: URLs to Web Capture content sets.
    Urls,
    /// `/EmbeddedFiles`: name strings to file specifications (§7.11.4).
    EmbeddedFiles,
    /// `/AlternatePresentations`: name strings to alternate presentations.
    AlternatePresentations,
    /// `/Renditions`: name strings to rendition objects (§13.2.3).
    Renditions,
}

impl NameTree {
    /// Every tree of Table 31, in the table's order.
    pub const ALL: [NameTree; 10] = [
        NameTree::Dests,
        NameTree::Ap,
        NameTree::JavaScript,
        NameTree::Pages,
        NameTree::Templates,
        NameTree::Ids,
        NameTree::Urls,
        NameTree::EmbeddedFiles,
        NameTree::AlternatePresentations,
        NameTree::Renditions,
    ];

    /// The tree's key in the name dictionary.
    pub fn key(self) -> &'static str {
        match self {
            NameTree::Dests => "Dests",
            NameTree::Ap => "AP",
            NameTree::JavaScript => "JavaScript",
            NameTree::Pages => "Pages",
            NameTree::Templates => "Templates",
            NameTree::Ids => "IDS",
            NameTree::Urls => "URLS",
            NameTree::EmbeddedFiles => "EmbeddedFiles",
            NameTree::AlternatePresentations => "AlternatePresentations",
            NameTree::Renditions => "Renditions",
        }
    }
}

/// The root node of `tree` in the catalog's name dictionary, or `None`
/// when the catalog, its `/Names` entry or the tree's entry is missing or
/// not a dictionary.
///
/// Covers ISO 32000-1 §7.7.4.
pub async fn name_tree_root_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    tree: NameTree,
) -> Option<Dict> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let names = resolved_dict(src, catalog.get("Names")?).await?;
    resolved_dict(src, names.get(tree.key())?).await
}

/// The object that `key` names in `tree`, resolved; `None` when the tree
/// or the name is absent.
///
/// Covers ISO 32000-1 §7.7.4.
pub async fn named_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    tree: NameTree,
    key: &[u8],
) -> Option<Object> {
    let root = name_tree_root_with(src, trailer, tree).await?;
    let value = tree::lookup(src, &root, &key.to_vec()).await?;
    src.resolve(&value).await.ok()
}

/// Every name in `tree` with the object it names, unresolved, in tree
/// order; empty when the tree is absent.
///
/// Covers ISO 32000-1 §7.7.4.
pub async fn names_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    tree: NameTree,
) -> Vec<(Vec<u8>, Object)> {
    match name_tree_root_with(src, trailer, tree).await {
        Some(root) => tree::entries(src, &root).await,
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Name, ObjRef};
    use crate::Document;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document whose catalog carries `catalog_extra`; `objects`
    /// supplies anything numbered 10 and up.
    fn doc(catalog_extra: &str, objects: &[(u32, &str)]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        for (num, body) in objects {
            b.object(*num, body);
        }
        Document::load(b.build(1)).expect("load")
    }

    fn keys(pairs: &[(Vec<u8>, Object)]) -> Vec<String> {
        pairs
            .iter()
            .map(|(k, _)| String::from_utf8(k.clone()).unwrap())
            .collect()
    }

    // Covers ISO 32000-1 §7.7.4.
    #[test]
    fn named_objects_come_from_the_catalog_name_dictionary() {
        // /Dests is an indirect two-level tree, /EmbeddedFiles a direct
        // single-leaf root inside the name dictionary itself.
        let doc = doc(
            "/Names 10 0 R",
            &[
                (
                    10,
                    "<< /Dests 11 0 R /EmbeddedFiles << /Names [(a.txt) 14 0 R] >> >>",
                ),
                (11, "<< /Kids [12 0 R 13 0 R] >>"),
                (
                    12,
                    "<< /Limits [(Chapter1) (Chapter2)] /Names [(Chapter1) [3 0 R /Fit] (Chapter2) 15 0 R] >>",
                ),
                (
                    13,
                    "<< /Limits [(Index) (Index)] /Names [(Index) [3 0 R /XYZ 0 0 null]] >>",
                ),
                (14, "<< /Type /Filespec /F (a.txt) >>"),
                (15, "[3 0 R /FitH 700]"),
            ],
        );
        let page = Object::Ref(ObjRef { num: 3, gen: 0 });
        assert_eq!(
            doc.named(NameTree::Dests, b"Chapter1"),
            Some(Object::Array(vec![
                page.clone(),
                Object::Name(Name("Fit".into()))
            ]))
        );
        // An indirect value comes back resolved.
        assert_eq!(
            doc.named(NameTree::Dests, b"Chapter2"),
            Some(Object::Array(vec![
                page,
                Object::Name(Name("FitH".into())),
                Object::Int(700)
            ]))
        );
        assert_eq!(doc.named(NameTree::Dests, b"Chapter3"), None);
        let spec = doc.named(NameTree::EmbeddedFiles, b"a.txt").unwrap();
        assert_eq!(
            spec.as_dict().unwrap().get("F"),
            Some(&Object::String(b"a.txt".to_vec()))
        );
        assert_eq!(
            keys(&doc.names(NameTree::Dests)),
            ["Chapter1", "Chapter2", "Index"]
        );
        assert_eq!(keys(&doc.names(NameTree::EmbeddedFiles)), ["a.txt"]);
        // A tree the dictionary does not carry.
        assert_eq!(doc.named(NameTree::JavaScript, b"init"), None);
        assert!(doc.names(NameTree::JavaScript).is_empty());
    }

    // Covers ISO 32000-1 §7.7.4.
    #[test]
    fn documents_without_a_name_dictionary_have_no_names() {
        let plain = doc("", &[]);
        assert_eq!(plain.named(NameTree::Dests, b"x"), None);
        assert!(plain.names(NameTree::Dests).is_empty());
        for tree in NameTree::ALL {
            assert!(plain.names(tree).is_empty(), "{tree:?}");
        }
        // A /Names entry that is not a dictionary, and a tree entry that
        // is not a dictionary, read as absent.
        let broken = doc("/Names 10 0 R", &[(10, "[1 2 3]")]);
        assert_eq!(broken.named(NameTree::Dests, b"x"), None);
        let broken = doc("/Names << /Dests (nope) >>", &[]);
        assert!(broken.names(NameTree::Dests).is_empty());
    }

    // Covers ISO 32000-1 §7.7.4.
    #[test]
    fn every_table_31_tree_has_its_dictionary_key() {
        let keys: Vec<&str> = NameTree::ALL.iter().map(|t| t.key()).collect();
        assert_eq!(
            keys,
            [
                "Dests",
                "AP",
                "JavaScript",
                "Pages",
                "Templates",
                "IDS",
                "URLS",
                "EmbeddedFiles",
                "AlternatePresentations",
                "Renditions"
            ]
        );
    }
}
