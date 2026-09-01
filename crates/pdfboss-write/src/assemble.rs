//! Merging documents into one fresh output (ISO 32000 §7.7.3, page tree):
//! selected pages from each source, gathered in argument order under a
//! single new `/Pages` node.

use pdfboss_core::{Dict, Document, Name, Object};

use crate::error::{Error, Result};
use crate::importer::Importer;
use crate::writer::{WriteOptions, Writer};

/// Assembles `inputs` into one document: each source's selected pages
/// (`None` takes every page), gathered in argument order under a fresh
/// `/Pages` node. The catalog and page tree are new; no `/Info` is set and
/// no `/ID` is inherited (the writer derives its own from the emitted
/// content). Document-level trees of the inputs -- outlines, names,
/// optional content -- are not carried, since only individual pages are
/// imported. An encrypted input is refused, the same way a lone import
/// would be.
pub fn merge_documents(
    inputs: &[(&Document, Option<&[usize]>)],
    options: WriteOptions,
) -> Result<Vec<u8>> {
    let mut writer = Writer::new(options);
    let pages_ref = writer.reserve();
    let mut kids = Vec::new();
    for (source, selection) in inputs {
        let mut importer = Importer::new(&mut writer, source)?;
        let indices: Vec<usize> = match selection {
            Some(indices) => indices.to_vec(),
            None => (0..source.page_count()).collect(),
        };
        for index in indices {
            kids.push(importer.page(index, pages_ref)?);
        }
    }
    if kids.is_empty() {
        return Err(Error::Other(
            "a document needs at least one page".to_string(),
        ));
    }
    let mut tree = Dict::new();
    tree.insert(name("Type"), Object::Name(name("Pages")));
    tree.insert(
        name("Kids"),
        Object::Array(kids.iter().copied().map(Object::Ref).collect()),
    );
    tree.insert(name("Count"), Object::Int(kids.len() as i64));
    writer.fill(pages_ref, Object::Dict(tree))?;
    let mut catalog = Dict::new();
    catalog.insert(name("Type"), Object::Name(name("Catalog")));
    catalog.insert(name("Pages"), Object::Ref(pages_ref));
    let root = writer.put(Object::Dict(catalog));
    writer.finish(root)
}

/// A `Name` from a string literal.
fn name(text: &str) -> Name {
    Name(text.to_string())
}

#[cfg(test)]
mod tests {
    use pdfboss_output::extract_text;
    use pdfboss_testkit::{encrypted_rc4_doc, multi_page_doc};

    use super::*;

    #[test]
    fn merge_keeps_sources_in_argument_order() {
        let a = Document::load(multi_page_doc(&["a1", "a2"])).expect("doc a loads");
        let b = Document::load(multi_page_doc(&["b1", "b2"])).expect("doc b loads");
        let bytes = merge_documents(&[(&a, None), (&b, None)], WriteOptions::default())
            .expect("merge succeeds");
        let merged = Document::load(bytes).expect("merged document loads");
        assert_eq!(merged.page_count(), 4);
        let texts: Vec<String> = (0..4)
            .map(|i| {
                let page = merged.page(i).expect("page exists");
                extract_text(&merged, &page).expect("text extracts")
            })
            .collect();
        assert!(texts[0].contains("a1"), "page 0: {:?}", texts[0]);
        assert!(texts[1].contains("a2"), "page 1: {:?}", texts[1]);
        assert!(texts[2].contains("b1"), "page 2: {:?}", texts[2]);
        assert!(texts[3].contains("b2"), "page 3: {:?}", texts[3]);
    }

    #[test]
    fn a_range_selects_and_reorders_pages() {
        let doc = Document::load(multi_page_doc(&["one", "two", "three"])).expect("doc loads");
        let bytes = merge_documents(&[(&doc, Some(&[2, 0]))], WriteOptions::default())
            .expect("merge succeeds");
        let merged = Document::load(bytes).expect("merged document loads");
        assert_eq!(merged.page_count(), 2);
        let first = merged.page(0).expect("first page exists");
        let second = merged.page(1).expect("second page exists");
        assert!(extract_text(&merged, &first).unwrap().contains("three"));
        assert!(extract_text(&merged, &second).unwrap().contains("one"));
    }

    #[test]
    fn encrypted_input_is_refused() {
        let doc = Document::load(encrypted_rc4_doc("secret")).expect("empty-password doc loads");
        let result = merge_documents(&[(&doc, None)], WriteOptions::default());
        assert!(matches!(result, Err(Error::EncryptedBase)));
    }
}
