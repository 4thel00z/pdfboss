//! Merging documents into one fresh output (ISO 32000 §7.7.3, page tree):
//! selected pages from each source, gathered in argument order under a
//! single new `/Pages` node.

use pdfboss_core::{Dict, Document, Name, Object};

use crate::error::{Error, Result};
use crate::importer::Importer;
use crate::update::core_error;
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

/// Rewrites `doc` fresh, like [`merge_documents`] but keeping the whole
/// document rather than assembling selected pages into a new tree: every
/// object the catalog and `/Info` reach is copied over, and each of
/// `pages` (0-based indices) gets its own leaf dictionary substituted with
/// `/Rotate` set to its current effective rotation plus `by`, normalized
/// with `rem_euclid(360)`. Substitution keys by the source object
/// reference, so a selected page with no object of its own (inlined
/// directly into `/Kids`) is refused, naming its 1-based page number:
/// pdfboss does not yet restructure such a page into one with its own
/// object.
pub fn rotate_rewrite(
    doc: &Document,
    pages: &[usize],
    by: i32,
    options: WriteOptions,
) -> Result<Vec<u8>> {
    let mut writer = Writer::new(options);
    let mut importer = Importer::new(&mut writer, doc)?;
    let new_info = doc
        .xref()
        .trailer
        .get_ref("Info")
        .map(|info| importer.reference(info));
    for &index in pages {
        let page = doc.page(index).map_err(core_error)?;
        let Some(page_ref) = page.object_ref() else {
            return Err(Error::Other(format!(
                "page {} is inlined into /Kids and cannot be edited in place; \
                 pdfboss does not yet restructure such pages to rotate them",
                index + 1
            )));
        };
        let mut dict = page.dict().clone();
        let rotate = (page.rotate + by).rem_euclid(360);
        dict.insert(name("Rotate"), Object::Int(i64::from(rotate)));
        let body = importer.copy(&Object::Dict(dict))?;
        importer.substitute(page_ref, body);
    }
    let new_root = importer.document()?;
    if let Some(new_info) = new_info {
        writer.set_info(new_info);
    }
    writer.finish(new_root)
}

/// Consecutive chunks of `every` pages, each a fresh document. `every` must
/// be at least 1; the last chunk carries whatever remains, so no chunk is
/// ever empty.
pub fn split_document(doc: &Document, every: usize, options: WriteOptions) -> Result<Vec<Vec<u8>>> {
    if every == 0 {
        return Err(Error::Other(
            "every must be at least 1 page per part".to_string(),
        ));
    }
    let total = doc.page_count();
    let mut parts = Vec::new();
    let mut start = 0;
    while start < total {
        let end = (start + every).min(total);
        let indices: Vec<usize> = (start..end).collect();
        parts.push(merge_documents(&[(doc, Some(&indices))], options)?);
        start = end;
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use pdfboss_output::extract_text;
    use pdfboss_testkit::{encrypted_rc4_doc, multi_page_doc, PdfBuilder};

    use crate::pdf::{Metadata, Page, PageSize, Pdf};

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

    #[test]
    fn split_makes_parts_of_the_requested_size_and_a_shorter_last_part() {
        let doc = Document::load(multi_page_doc(&["one", "two", "three"])).expect("doc loads");
        let parts = split_document(&doc, 2, WriteOptions::default()).expect("split succeeds");
        assert_eq!(parts.len(), 2);

        let first = Document::load(parts[0].clone()).expect("first part loads");
        assert_eq!(first.page_count(), 2);
        let texts: Vec<String> = (0..2)
            .map(|i| {
                let page = first.page(i).expect("page exists");
                extract_text(&first, &page).expect("text extracts")
            })
            .collect();
        assert!(texts[0].contains("one"), "page 0: {:?}", texts[0]);
        assert!(texts[1].contains("two"), "page 1: {:?}", texts[1]);

        let second = Document::load(parts[1].clone()).expect("second part loads");
        assert_eq!(second.page_count(), 1);
        let page = second.page(0).expect("page exists");
        let text = extract_text(&second, &page).expect("text extracts");
        assert!(text.contains("three"), "page 0: {:?}", text);
    }

    #[test]
    fn split_larger_than_the_page_count_makes_one_part() {
        let doc = Document::load(multi_page_doc(&["one", "two", "three"])).expect("doc loads");
        let parts = split_document(&doc, 10, WriteOptions::default()).expect("split succeeds");
        assert_eq!(parts.len(), 1);
        let only = Document::load(parts[0].clone()).expect("part loads");
        assert_eq!(only.page_count(), 3);
    }

    #[test]
    fn split_rejects_zero_pages_per_part_with_an_honest_message() {
        let doc = Document::load(multi_page_doc(&["one", "two", "three"])).expect("doc loads");
        let result = split_document(&doc, 0, WriteOptions::default());
        let Err(Error::Other(message)) = result else {
            panic!("expected Error::Other, got {result:?}");
        };
        assert!(message.contains("every"), "message: {message}");
    }

    /// Rotating pages 1 and 3 of a three-page document by 90 degrees
    /// clockwise substitutes each page's own object with its effective
    /// rotation plus 90, leaving the untouched page at 0. Unlike the
    /// append path, the whole document is copied fresh.
    #[test]
    fn rotate_rewrite_rotates_the_selected_pages() {
        let doc = Document::load(multi_page_doc(&["one", "two", "three"])).expect("doc loads");
        let bytes =
            rotate_rewrite(&doc, &[0, 2], 90, WriteOptions::default()).expect("rotate succeeds");
        let rotated = Document::load(bytes).expect("rotated document loads");
        for (index, expected) in [90, 0, 90].iter().enumerate() {
            let page = rotated.page(index).expect("page exists");
            assert_eq!(page.rotate, *expected, "page {index}");
        }
    }

    /// A page inlined directly into `/Kids`, with no object of its own, has
    /// no reference to substitute a rewritten body onto: `rotate_rewrite`
    /// refuses it, naming its 1-based page number, rather than silently
    /// leaving it unrotated.
    #[test]
    fn rotate_rewrite_refuses_an_inline_page() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(
            2,
            "<< /Type /Pages /Count 1 /Kids [ << /Type /Page /Parent 2 0 R \
             /MediaBox [0 0 612 792] >> ] >>",
        );
        let doc = Document::load(b.build(1)).expect("doc loads");

        let result = rotate_rewrite(&doc, &[0], 90, WriteOptions::default());
        let Err(Error::Other(message)) = result else {
            panic!("expected Error::Other, got {result:?}");
        };
        assert!(message.contains("page 1"), "message: {message}");
        assert!(
            message.contains("cannot be edited in place"),
            "message: {message}"
        );
        assert!(
            message.contains("does not yet restructure"),
            "message: {message}"
        );
    }

    /// A rewrite carries `/Info` along: the reloaded catalog's trailer
    /// still resolves an `/Info` dictionary, and its `/Title` still reads
    /// the base document's title after rotation.
    #[test]
    fn rotate_rewrite_carries_info_along() {
        let base = Pdf {
            pages: vec![Page::new(PageSize::A4)],
            metadata: Some(Metadata {
                title: Some("Rotated Title".to_string()),
                ..Metadata::default()
            }),
            ..Pdf::default()
        }
        .to_bytes()
        .expect("base builds");
        let doc = Document::load(base).expect("base loads");
        assert!(
            doc.xref().trailer.get_ref("Info").is_some(),
            "the base's trailer must carry /Info for this test to exercise the carry"
        );

        let bytes =
            rotate_rewrite(&doc, &[0], 90, WriteOptions::default()).expect("rotate succeeds");
        let rotated = Document::load(bytes).expect("rotated document loads");
        assert!(
            rotated.xref().trailer.get_ref("Info").is_some(),
            "the rewritten trailer still names an /Info dictionary"
        );
        assert_eq!(rotated.metadata().title.as_deref(), Some("Rotated Title"));
    }
}
