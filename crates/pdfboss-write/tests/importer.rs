//! The public `Importer`: per-source object-graph transplant into a
//! [`Writer`], exercised directly rather than through `watermark_with`.

use pdfboss_core::{Dict, Document, Name, ObjRef, Object, Rect, Stream};
use pdfboss_output::extract_text;
use pdfboss_testkit::{encrypted_rc4_doc, multi_page_doc, PdfBuilder};
use pdfboss_write::{Error, Importer, WriteOptions, Writer, XrefStyle};

fn name(text: &str) -> Name {
    Name(text.to_string())
}

/// Wraps `page_refs` in a one-level `/Pages` tree at `pages_ref`, builds a
/// catalog over it, and finishes `writer` into file bytes.
fn finish_with_tree(mut writer: Writer, pages_ref: ObjRef, page_refs: &[ObjRef]) -> Vec<u8> {
    let mut tree = Dict::new();
    tree.insert(name("Type"), Object::Name(name("Pages")));
    tree.insert(
        name("Kids"),
        Object::Array(page_refs.iter().map(|r| Object::Ref(*r)).collect()),
    );
    tree.insert(name("Count"), Object::Int(page_refs.len() as i64));
    writer
        .fill(pages_ref, Object::Dict(tree))
        .expect("the reserved /Pages ref is fillable");
    let mut catalog = Dict::new();
    catalog.insert(name("Type"), Object::Name(name("Catalog")));
    catalog.insert(name("Pages"), Object::Ref(pages_ref));
    let root = writer.put(Object::Dict(catalog));
    writer
        .finish(root)
        .expect("the assembled document finishes")
}

/// Raw, uncompressed, no-object-streams options: output stays literal text
/// so a test can grep for a substring.
fn plain_table_options() -> WriteOptions {
    WriteOptions {
        xref: XrefStyle::Table,
        compress: false,
        object_streams: false,
        version: (1, 7),
    }
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[test]
fn imported_page_is_self_contained() {
    let base = multi_page_doc(&["one", "two", "three"]);
    let doc = Document::load(base).expect("base document loads");
    let mut writer = Writer::new(WriteOptions::default());
    let pages_ref = writer.reserve();
    let page_ref = {
        let mut importer = Importer::new(&mut writer, &doc).expect("an unencrypted source opens");
        importer.page(1, pages_ref).expect("page 1 imports")
    };
    let bytes = finish_with_tree(writer, pages_ref, &[page_ref]);

    let reloaded = Document::load(bytes).expect("assembled document loads");
    assert_eq!(reloaded.page_count(), 1);
    let page = reloaded.page(0).expect("the one page exists");
    let text = extract_text(&reloaded, &page).expect("text extracts");
    assert!(text.contains("two"), "unexpected text: {text:?}");
    assert!(page.dict().get("Resources").is_some());
    assert!(page.dict().get("MediaBox").is_some());
    assert_eq!(page.dict().get_ref("Parent"), Some(pages_ref));
}

#[test]
fn inherited_attributes_survive_the_transplant() {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] \
         /Resources << /Font << /F1 5 0 R >> >> >>",
    );
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /Rotate 90 /Contents 4 0 R >>",
    );
    b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (leaf) Tj ET");
    b.object(
        5,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );
    let base = b.build(1);
    let doc = Document::load(base).expect("base document loads");

    let mut writer = Writer::new(WriteOptions::default());
    let pages_ref = writer.reserve();
    let page_ref = {
        let mut importer = Importer::new(&mut writer, &doc).expect("an unencrypted source opens");
        importer.page(0, pages_ref).expect("the leaf page imports")
    };
    let bytes = finish_with_tree(writer, pages_ref, &[page_ref]);

    let reloaded = Document::load(bytes).expect("assembled document loads");
    let page = reloaded.page(0).expect("the one page exists");
    assert_eq!(page.media_box, Rect::new(0.0, 0.0, 300.0, 400.0));
    assert_eq!(page.rotate, 90);
}

#[test]
fn shared_resources_dedup_across_pages() {
    let base = multi_page_doc(&["one", "two", "three"]);
    let doc = Document::load(base).expect("base document loads");
    let mut writer = Writer::new(plain_table_options());
    let pages_ref = writer.reserve();
    let (page0, page2) = {
        let mut importer = Importer::new(&mut writer, &doc).expect("an unencrypted source opens");
        let page0 = importer.page(0, pages_ref).expect("page 0 imports");
        let page2 = importer.page(2, pages_ref).expect("page 2 imports");
        (page0, page2)
    };
    let bytes = finish_with_tree(writer, pages_ref, &[page0, page2]);

    let count = count_occurrences(&bytes, b"/Type /Font");
    assert_eq!(count, 1, "the shared font must appear exactly once");
}

#[test]
fn substitute_swaps_a_body_verbatim() {
    let base = multi_page_doc(&["one", "two", "three"]);
    let doc = Document::load(base).expect("base document loads");
    let content_ref = match doc.page(1).expect("page 1 exists").dict().get("Contents") {
        Some(Object::Ref(r)) => *r,
        other => panic!("expected an indirect /Contents, got {other:?}"),
    };

    let mut writer = Writer::new(WriteOptions::default());
    let root = {
        let mut importer = Importer::new(&mut writer, &doc).expect("an unencrypted source opens");
        importer.substitute(
            content_ref,
            Object::Stream(Stream {
                dict: Dict::new(),
                data: b"BT /F1 12 Tf 72 720 Td (SWAPPED) Tj ET".to_vec(),
            }),
        );
        importer.document().expect("the whole graph imports")
    };
    let bytes = writer
        .finish(root)
        .expect("the assembled document finishes");

    let reloaded = Document::load(bytes).expect("assembled document loads");
    assert_eq!(reloaded.page_count(), 3);
    let texts: Vec<String> = (0..3)
        .map(|i| {
            let page = reloaded.page(i).expect("page exists");
            extract_text(&reloaded, &page).expect("text extracts")
        })
        .collect();
    assert!(texts[0].contains("one"), "page 0: {:?}", texts[0]);
    assert!(texts[1].contains("SWAPPED"), "page 1: {:?}", texts[1]);
    assert!(!texts[1].contains("two"), "page 1: {:?}", texts[1]);
    assert!(texts[2].contains("three"), "page 2: {:?}", texts[2]);
}

#[test]
fn copy_translates_source_refs_into_the_target() {
    let base = multi_page_doc(&["one"]);
    let doc = Document::load(base).expect("base document loads");
    let font_ref = ObjRef { num: 3, gen: 0 };

    let mut writer = Writer::new(WriteOptions::default());
    let (new_font_ref, root) = {
        let mut importer = Importer::new(&mut writer, &doc).expect("an unencrypted source opens");
        let mut source = Dict::new();
        source.insert(name("Font"), Object::Ref(font_ref));
        let copied = importer
            .copy(&Object::Dict(source))
            .expect("copy translates the dict");
        let new_font_ref = match copied {
            Object::Dict(d) => d.get_ref("Font").expect("the Font entry survives"),
            other => panic!("expected a dict, got {other:?}"),
        };
        assert_ne!(new_font_ref.num, font_ref.num);
        let root = importer.document().expect("the whole graph imports");
        (new_font_ref, root)
    };
    let bytes = writer
        .finish(root)
        .expect("the assembled document finishes");

    let reloaded = Document::load(bytes).expect("assembled document loads");
    let font = reloaded.get(new_font_ref).expect("the font resolves");
    assert_eq!(
        font.as_dict().expect("font is a dict").get_name("Type"),
        Some(&Name("Font".to_string()))
    );
}

#[test]
fn document_import_reaches_the_whole_graph() {
    let base = multi_page_doc(&["one", "two", "three"]);
    let doc = Document::load(base).expect("base document loads");

    let mut writer = Writer::new(WriteOptions::default());
    let root = {
        let mut importer = Importer::new(&mut writer, &doc).expect("an unencrypted source opens");
        importer.document().expect("the whole graph imports")
    };
    let bytes = writer
        .finish(root)
        .expect("the assembled document finishes");

    let reloaded = Document::load(bytes).expect("assembled document loads");
    assert_eq!(reloaded.page_count(), 3);
    for (index, expected) in ["one", "two", "three"].iter().enumerate() {
        let page = reloaded.page(index).expect("page exists");
        let text = extract_text(&reloaded, &page).expect("text extracts");
        assert!(text.contains(expected), "page {index}: {text:?}");
    }
}

#[test]
fn encrypted_source_is_refused() {
    let base = encrypted_rc4_doc("secret message");
    let doc = Document::load(base).expect("empty-password RC4 document loads");
    let mut writer = Writer::new(WriteOptions::default());
    let result = Importer::new(&mut writer, &doc);
    assert!(matches!(result, Err(Error::EncryptedBase)));
}

#[test]
fn inline_page_gets_its_own_object() {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(
        2,
        "<< /Type /Pages /Count 1 /Kids [ << /Type /Page /Parent 2 0 R \
         /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> \
         /Contents 4 0 R >> ] >>",
    );
    b.object(
        3,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );
    b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (inline) Tj ET");
    let base = b.build(1);
    let doc = Document::load(base).expect("base document loads");
    assert_eq!(
        doc.page(0).expect("page exists").object_ref(),
        None,
        "the fixture page must start inlined into /Kids"
    );

    let mut writer = Writer::new(WriteOptions::default());
    let pages_ref = writer.reserve();
    let page_ref = {
        let mut importer = Importer::new(&mut writer, &doc).expect("an unencrypted source opens");
        importer
            .page(0, pages_ref)
            .expect("the inline page imports")
    };
    let bytes = finish_with_tree(writer, pages_ref, &[page_ref]);

    let reloaded = Document::load(bytes).expect("assembled document loads");
    let page = reloaded.page(0).expect("the one page exists");
    assert_eq!(
        page.object_ref(),
        Some(page_ref),
        "the imported page must now be an indirect object"
    );
    let text = extract_text(&reloaded, &page).expect("text extracts");
    assert!(text.contains("inline"), "unexpected text: {text:?}");
}
