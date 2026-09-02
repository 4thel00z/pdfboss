//! Merging documents into one fresh output (ISO 32000 §7.7.3, page tree):
//! selected pages from each source, gathered in argument order under a
//! single new `/Pages` node.

use pdfboss_core::{Dict, Document, Name, Object};

use crate::error::{Error, Result};
use crate::importer::Importer;
use crate::pdf::Metadata;
use crate::update::{
    catalog_metadata_ref, core_error, merge_metadata, resolve_dict, xmp_metadata_stream,
};
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
/// object. `by` must be a multiple of 90; anything else is refused before
/// any object is copied.
pub fn rotate_rewrite(
    doc: &Document,
    pages: &[usize],
    by: i32,
    options: WriteOptions,
) -> Result<Vec<u8>> {
    if by % 90 != 0 {
        return Err(Error::Other(
            "rotation must be a multiple of 90 degrees".to_string(),
        ));
    }
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

/// The whole document through the [`Writer`]: recompressed, object streams
/// per `options`, unreachable objects and earlier update sections left
/// behind. Carries `/Info` along the same way [`rotate_rewrite`] does: it
/// is a trailer key `Importer::document` alone can never reach, since
/// nothing in the catalog's own graph points at it.
pub fn rewrite_document(doc: &Document, options: WriteOptions) -> Result<Vec<u8>> {
    let mut writer = Writer::new(options);
    let mut importer = Importer::new(&mut writer, doc)?;
    let new_info = doc
        .xref()
        .trailer
        .get_ref("Info")
        .map(|info| importer.reference(info));
    let new_root = importer.document()?;
    if let Some(new_info) = new_info {
        writer.set_info(new_info);
    }
    writer.finish(new_root)
}

/// [`rewrite_document`], first replacing `/Info` (and, when the catalog
/// names one, the XMP packet) with `meta` merged over whatever the base
/// already carried: the same merge [`crate::update::set_metadata_with`]
/// performs for an appended update, applied here as substitutions into the
/// copied graph instead. An existing `/Info` object is translated into the
/// target's own numbering via [`Importer::copy`] before the substitution
/// (`resolve_dict` only chases a value's own top-level reference, so a
/// nested or unresolvable one still names a source object; `copy`
/// translates it correctly, the same pattern [`rotate_rewrite`] uses for a
/// page body). A base with no `/Info` gets a fresh one put directly into
/// the writer, since there is no source object to substitute into and
/// nothing in a freshly built dict can name one.
pub fn rewrite_with_metadata(
    doc: &Document,
    meta: Metadata,
    options: WriteOptions,
) -> Result<Vec<u8>> {
    let mut writer = Writer::new(options);
    let mut importer = Importer::new(&mut writer, doc)?;
    let trailer = &doc.xref().trailer;
    let root = trailer.get_ref("Root").ok_or(Error::MissingRoot)?;
    let info_ref = trailer.get_ref("Info");
    let existing_dict = info_ref.and_then(|r| {
        let dict = doc.get(r).ok()?.as_dict()?.clone();
        Some(resolve_dict(doc, &dict))
    });
    let xmp_ref = catalog_metadata_ref(doc, root);
    let (dict, merged) = merge_metadata(existing_dict, &meta);

    let new_info_target = match info_ref {
        Some(r) => {
            let target = importer.reference(r);
            let body = importer.copy(&Object::Dict(dict.clone()))?;
            importer.substitute(r, body);
            Some(target)
        }
        None => None,
    };
    if let Some(r) = xmp_ref {
        importer.substitute(r, xmp_metadata_stream(&merged));
    }

    let new_root = importer.document()?;
    let new_info = match new_info_target {
        Some(target) => target,
        None => writer.put(Object::Dict(dict)),
    };
    writer.set_info(new_info);
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
    use pdfboss_core::xref::{parse_section_at, startxref, XrefEntry};
    use pdfboss_output::extract_text;
    use pdfboss_testkit::{encrypted_rc4_doc, multi_page_doc, PdfBuilder};

    use crate::pdf::{Metadata, Page, PageSize, Pdf};
    use crate::update::Update;
    use crate::writer::XrefStyle;

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

    /// A `by` that is not a multiple of 90 is refused before any object is
    /// copied, rather than silently truncated or wrapped into a confusing
    /// rotation.
    #[test]
    fn rotate_rewrite_refuses_a_non_multiple_of_90() {
        let doc = Document::load(multi_page_doc(&["one"])).expect("doc loads");
        let result = rotate_rewrite(&doc, &[0], 45, WriteOptions::default());
        let Err(Error::Other(message)) = result else {
            panic!("expected Error::Other, got {result:?}");
        };
        assert!(message.contains("multiple of 90"), "message: {message}");
    }

    /// A negative multiple of 90 stays legal: `rem_euclid(360)` normalizes
    /// it into the usual 0..360 range instead of refusing it.
    #[test]
    fn rotate_rewrite_accepts_a_negative_multiple_of_90() {
        let doc = Document::load(multi_page_doc(&["one"])).expect("doc loads");
        let bytes =
            rotate_rewrite(&doc, &[0], -90, WriteOptions::default()).expect("rotate succeeds");
        let rotated = Document::load(bytes).expect("rotated document loads");
        let page = rotated.page(0).expect("page exists");
        assert_eq!(page.rotate, 270);
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

    /// Counts non-free cross-reference entries: the objects a document
    /// actually carries, whether stored directly or packed into an object
    /// stream.
    fn live_count(doc: &Document) -> usize {
        doc.xref()
            .iter()
            .filter(|(_, entry)| !matches!(entry, XrefEntry::Free))
            .count()
    }

    /// A rewrite carries `/Info` along, the same way [`rotate_rewrite`]
    /// does: the reloaded trailer still resolves an `/Info` dictionary, and
    /// its `/Title` still reads the base document's title.
    #[test]
    fn rewrite_document_carries_info_along() {
        let base = Pdf {
            pages: vec![Page::new(PageSize::A4)],
            metadata: Some(Metadata {
                title: Some("Rewritten Title".to_string()),
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

        let bytes = rewrite_document(&doc, WriteOptions::default()).expect("rewrite succeeds");
        let rewritten = Document::load(bytes).expect("rewritten document loads");
        assert!(
            rewritten.xref().trailer.get_ref("Info").is_some(),
            "the rewritten trailer still names an /Info dictionary"
        );
        assert_eq!(
            rewritten.metadata().title.as_deref(),
            Some("Rewritten Title")
        );
    }

    /// A rewrite recomputes the whole object graph from the catalog and
    /// `/Info` alone: an object neither one reaches is dropped, even though
    /// the base carried it, and the pages that remain still read back.
    #[test]
    fn rewrite_document_drops_an_unreferenced_object_and_keeps_text() {
        let options = WriteOptions {
            xref: XrefStyle::Table,
            compress: false,
            object_streams: false,
            version: (1, 7),
        };
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (hello) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
        b.object(6, "<< /Extra (unreferenced) >>");
        let doc = Document::load(b.build(1)).expect("doc loads");
        assert_eq!(
            live_count(&doc),
            6,
            "the fixture carries the extra object alongside the reachable five"
        );

        let bytes = rewrite_document(&doc, options).expect("rewrite succeeds");
        let rewritten = Document::load(bytes).expect("rewritten document loads");
        assert_eq!(
            live_count(&rewritten),
            5,
            "the unreferenced object must not survive the rewrite"
        );

        let page = rewritten.page(0).expect("page exists");
        let text = extract_text(&rewritten, &page).expect("text extracts");
        assert!(text.contains("hello"), "text: {text:?}");
    }

    /// A base already carrying an appended update section (two
    /// cross-reference sections chained by `/Prev`) collapses to one fresh
    /// section: a rewrite always builds a whole new graph, never an append
    /// of its own.
    #[test]
    fn rewrite_document_collapses_an_appended_update_into_one_section() {
        let base = Pdf {
            pages: vec![Page::new(PageSize::A4)],
            ..Pdf::default()
        }
        .to_bytes()
        .expect("base builds");
        let base_doc = Document::load(base).expect("base loads");
        let mut update = Update::new(&base_doc).expect("update opens");
        let extra = update.reserve();
        let mut dict = Dict::new();
        dict.insert(name("Marker"), Object::Int(1));
        update.set(extra, Object::Dict(dict));
        let appended = update.bytes().expect("update appends");

        let control_offset = startxref(&appended).expect("startxref present in the input");
        let control_section =
            parse_section_at(&appended, control_offset).expect("input section parses");
        assert!(
            control_section.prev.is_some(),
            "the input must really carry a /Prev chain for this test to exercise the collapse"
        );

        let appended_doc = Document::load(appended).expect("appended document loads");
        let bytes =
            rewrite_document(&appended_doc, WriteOptions::default()).expect("rewrite succeeds");

        let offset = startxref(&bytes).expect("startxref present");
        let section = parse_section_at(&bytes, offset).expect("section parses");
        assert!(
            section.prev.is_none(),
            "the rewrite must collapse the update chain into one section"
        );
    }

    /// Rewriting with metadata merges `meta`'s `Some` fields over whatever
    /// the base already carried, the same as an appended `set_metadata`,
    /// but into a whole fresh file: the output cannot start with the
    /// base's own bytes, since there is no append to preserve a prefix of.
    #[test]
    fn rewrite_with_metadata_merges_fields_into_a_fresh_file() {
        let base = Pdf {
            pages: vec![Page::new(PageSize::A4)],
            metadata: Some(Metadata {
                title: Some("Old".to_string()),
                author: Some("Keep".to_string()),
                ..Metadata::default()
            }),
            ..Pdf::default()
        }
        .to_bytes()
        .expect("base builds");
        let doc = Document::load(base.clone()).expect("base loads");

        let bytes = rewrite_with_metadata(
            &doc,
            Metadata {
                title: Some("New".to_string()),
                ..Metadata::default()
            },
            WriteOptions::default(),
        )
        .expect("rewrite succeeds");
        assert!(
            !bytes.starts_with(&base[..]),
            "a metadata rewrite must not merely append an update onto the base"
        );

        let rewritten = Document::load(bytes).expect("rewritten document loads");
        let meta = rewritten.metadata();
        assert_eq!(meta.title.as_deref(), Some("New"));
        assert_eq!(meta.author.as_deref(), Some("Keep"));

        let new_root = rewritten
            .xref()
            .trailer
            .get_ref("Root")
            .expect("rewritten trailer names /Root");
        let catalog = rewritten.get(new_root).expect("catalog resolves");
        let metadata_ref = catalog
            .as_dict()
            .expect("catalog is a dictionary")
            .get_ref("Metadata")
            .expect("the base's XMP packet must still be named");
        let stream = rewritten
            .get(metadata_ref)
            .expect("metadata stream resolves");
        let text = String::from_utf8(
            stream
                .as_stream()
                .expect("metadata is a stream")
                .data
                .clone(),
        )
        .expect("packet is utf-8");
        assert!(text.contains("New"), "packet: {text}");
        assert!(text.contains("Keep"), "packet: {text}");
        assert!(!text.contains("Old"), "packet: {text}");
    }

    /// A base with no `/Info` at all still gets one from
    /// `rewrite_with_metadata`: the merge target is a fresh object put
    /// directly into the writer, never an `Importer` substitution. A base
    /// with no XMP packet either must not gain one: `set_metadata_with`'s
    /// own rule (never build a fresh packet where none existed) applies
    /// here too.
    #[test]
    fn rewrite_with_metadata_creates_info_when_absent() {
        let base = Pdf {
            pages: vec![Page::new(PageSize::A4)],
            ..Pdf::default()
        }
        .to_bytes()
        .expect("base builds");
        let doc = Document::load(base).expect("base loads");

        let bytes = rewrite_with_metadata(
            &doc,
            Metadata {
                title: Some("Fresh".to_string()),
                ..Metadata::default()
            },
            WriteOptions::default(),
        )
        .expect("rewrite succeeds");

        let rewritten = Document::load(bytes).expect("rewritten document loads");
        assert_eq!(rewritten.metadata().title.as_deref(), Some("Fresh"));

        let new_root = rewritten
            .xref()
            .trailer
            .get_ref("Root")
            .expect("rewritten trailer names /Root");
        let catalog = rewritten.get(new_root).expect("catalog resolves");
        assert!(
            catalog
                .as_dict()
                .expect("catalog is a dictionary")
                .get("Metadata")
                .is_none(),
            "a base with no XMP packet must not gain one from a metadata rewrite"
        );
    }

    /// A kept `/Info` value stored as an indirect reference must be
    /// translated into the target's own numbering, not carried verbatim:
    /// `resolve_dict` only chases a value's own top-level reference chain,
    /// so the merged dict still names the source object directly, and
    /// `Importer::substitute` fills bodies verbatim with no renumbering of
    /// its own. Left untranslated, the raw source number would alias
    /// whatever the target happens to number the same in the rewritten
    /// file.
    #[test]
    fn rewrite_with_metadata_translates_a_kept_indirect_info_value() {
        let mut w = Writer::new(WriteOptions {
            xref: XrefStyle::Table,
            ..WriteOptions::default()
        });
        let pages_root = w.reserve();
        let page = w.reserve();

        let mut page_dict = Dict::new();
        page_dict.insert(name("Type"), Object::Name(name("Page")));
        page_dict.insert(name("Parent"), Object::Ref(pages_root));
        page_dict.insert(name("Resources"), Object::Dict(Dict::new()));
        page_dict.insert(
            name("MediaBox"),
            Object::Array(vec![
                Object::Int(0),
                Object::Int(0),
                Object::Int(612),
                Object::Int(792),
            ]),
        );
        w.fill(page, Object::Dict(page_dict))
            .expect("page slot fills");

        let mut pages_dict = Dict::new();
        pages_dict.insert(name("Type"), Object::Name(name("Pages")));
        pages_dict.insert(name("Kids"), Object::Array(vec![Object::Ref(page)]));
        pages_dict.insert(name("Count"), Object::Int(1));
        w.fill(pages_root, Object::Dict(pages_dict))
            .expect("pages slot fills");

        let title_ref = w.put(Object::String(b"Indirect Title".to_vec()));
        let mut info = Dict::new();
        info.insert(name("Title"), Object::Ref(title_ref));
        let info_ref = w.put(Object::Dict(info));
        w.set_info(info_ref);

        let mut catalog = Dict::new();
        catalog.insert(name("Type"), Object::Name(name("Catalog")));
        catalog.insert(name("Pages"), Object::Ref(pages_root));
        let root = w.put(Object::Dict(catalog));
        let base = w.finish(root).expect("base finishes");
        let doc = Document::load(base).expect("base loads");

        let bytes = rewrite_with_metadata(
            &doc,
            Metadata {
                author: Some("New Author".to_string()),
                ..Metadata::default()
            },
            WriteOptions::default(),
        )
        .expect("rewrite succeeds");

        let rewritten = Document::load(bytes).expect("rewritten document loads");
        let meta = rewritten.metadata();
        assert_eq!(
            meta.title.as_deref(),
            Some("Indirect Title"),
            "a kept indirect /Info value must translate rather than alias"
        );
        assert_eq!(meta.author.as_deref(), Some("New Author"));

        let page = rewritten.page(0).expect("page still resolves");
        assert_eq!(
            page.dict().get_name("Type"),
            Some(&Name("Page".to_string())),
            "the page object must not have been aliased by an untranslated /Info reference"
        );
    }

    /// `resolve_dict` resolves a key's own top-level reference chain, but
    /// never recurses into a value that is itself an array or a nested
    /// dictionary: a reference held inside one survives the merge
    /// untouched, still naming a source object. `rewrite_with_metadata`
    /// must translate it into the target's own numbering rather than
    /// substituting it verbatim, or the raw source number would alias
    /// whatever the rewrite happens to number the same.
    #[test]
    fn rewrite_with_metadata_translates_a_reference_nested_in_an_info_value() {
        let mut w = Writer::new(WriteOptions {
            xref: XrefStyle::Table,
            ..WriteOptions::default()
        });
        let pages_root = w.reserve();
        let page = w.reserve();

        let mut page_dict = Dict::new();
        page_dict.insert(name("Type"), Object::Name(name("Page")));
        page_dict.insert(name("Parent"), Object::Ref(pages_root));
        page_dict.insert(name("Resources"), Object::Dict(Dict::new()));
        page_dict.insert(
            name("MediaBox"),
            Object::Array(vec![
                Object::Int(0),
                Object::Int(0),
                Object::Int(612),
                Object::Int(792),
            ]),
        );
        w.fill(page, Object::Dict(page_dict))
            .expect("page slot fills");

        let mut pages_dict = Dict::new();
        pages_dict.insert(name("Type"), Object::Name(name("Pages")));
        pages_dict.insert(name("Kids"), Object::Array(vec![Object::Ref(page)]));
        pages_dict.insert(name("Count"), Object::Int(1));
        w.fill(pages_root, Object::Dict(pages_dict))
            .expect("pages slot fills");

        let witness = w.put(Object::String(b"Witness".to_vec()));
        let mut info = Dict::new();
        info.insert(name("Title"), Object::String(b"Plain Title".to_vec()));
        info.insert(
            name("CustomRefs"),
            Object::Array(vec![Object::Ref(witness)]),
        );
        let info_ref = w.put(Object::Dict(info));
        w.set_info(info_ref);

        let mut catalog = Dict::new();
        catalog.insert(name("Type"), Object::Name(name("Catalog")));
        catalog.insert(name("Pages"), Object::Ref(pages_root));
        let root = w.put(Object::Dict(catalog));
        let base = w.finish(root).expect("base finishes");
        let doc = Document::load(base).expect("base loads");

        let bytes = rewrite_with_metadata(
            &doc,
            Metadata {
                author: Some("New Author".to_string()),
                ..Metadata::default()
            },
            WriteOptions::default(),
        )
        .expect("rewrite succeeds");

        let rewritten = Document::load(bytes).expect("rewritten document loads");
        let new_info_ref = rewritten
            .xref()
            .trailer
            .get_ref("Info")
            .expect("rewritten trailer names /Info");
        let info_dict = rewritten.get(new_info_ref).expect("info resolves");
        let custom = info_dict
            .as_dict()
            .expect("info is a dictionary")
            .get("CustomRefs")
            .expect("CustomRefs survives the merge, untouched by the recognized fields");
        let Object::Array(items) = custom else {
            panic!("CustomRefs must still be an array, got {custom:?}");
        };
        let Object::Ref(witness_target) = items[0] else {
            panic!(
                "CustomRefs[0] must still be a reference, got {:?}",
                items[0]
            );
        };
        let resolved = rewritten
            .get(witness_target)
            .expect("the translated reference must resolve to a real object");
        assert_eq!(
            resolved.as_str_bytes(),
            Some(&b"Witness"[..]),
            "a reference nested inside an /Info value must translate into the \
             target's own numbering, not alias whatever the rewrite happens to \
             number the same"
        );
    }
}
