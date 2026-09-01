//! The public update-append API: `OverlayBase`, `Overlay` and `Update` over
//! an existing document, exercised directly rather than through
//! `watermark`.

use pdfboss_core::xref::{parse_section_at, startxref};
use pdfboss_core::{Dict, Document, Name, ObjRef, Object};
use pdfboss_write::{
    Error, OverlayBase, Page, PageSize, Pdf, Standard14, Update, WriteOptions, XrefStyle,
};

fn base_pdf(xref: XrefStyle) -> Vec<u8> {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("Base page", 72.0, 700.0, Standard14::Helvetica, 14.0)
        .unwrap();
    Pdf {
        pages: vec![page],
        options: WriteOptions {
            xref,
            ..WriteOptions::default()
        },
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap()
}

fn classic_base() -> Vec<u8> {
    base_pdf(XrefStyle::Table)
}

fn stream_base() -> Vec<u8> {
    base_pdf(XrefStyle::Stream)
}

#[test]
fn set_replaces_object_classic() {
    let base = classic_base();
    let doc = Document::load(base.clone()).unwrap();
    let mut update = Update::new(&doc).unwrap();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(7));
    update.set(ObjRef { num: 1, gen: 0 }, Object::Dict(dict));
    let out = update.appended().unwrap();
    assert_eq!(&out[..base.len()], &base[..]);
    let reread = Document::load(out).unwrap();
    assert_eq!(
        reread
            .get(ObjRef { num: 1, gen: 0 })
            .unwrap()
            .as_dict()
            .unwrap()
            .get_int("Marker"),
        Some(7)
    );
}

#[test]
fn set_replaces_object_stream() {
    let base = stream_base();
    let doc = Document::load(base.clone()).unwrap();
    let mut update = Update::new(&doc).unwrap();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(7));
    update.set(ObjRef { num: 1, gen: 0 }, Object::Dict(dict));
    let out = update.appended().unwrap();
    assert_eq!(&out[..base.len()], &base[..]);
    let reread = Document::load(out).unwrap();
    assert_eq!(
        reread
            .get(ObjRef { num: 1, gen: 0 })
            .unwrap()
            .as_dict()
            .unwrap()
            .get_int("Marker"),
        Some(7)
    );
}

#[test]
fn reserve_allocates_past_base_size() {
    let base = classic_base();
    let doc = Document::load(base).unwrap();
    let mut update = Update::new(&doc).unwrap();
    let r = update.reserve();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(9));
    update.set(r, Object::Dict(dict));
    let out = update.appended().unwrap();
    let reread = Document::load(out).unwrap();
    assert_eq!(
        reread.get(r).unwrap().as_dict().unwrap().get_int("Marker"),
        Some(9)
    );
}

#[test]
fn two_appends_chain() {
    let base = classic_base();
    let doc1 = Document::load(base).unwrap();
    let mut update1 = Update::new(&doc1).unwrap();
    let mut first = Dict::new();
    first.insert(Name("First".into()), Object::Int(1));
    update1.set(ObjRef { num: 1, gen: 0 }, Object::Dict(first));
    let once = update1.appended().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    let extra = update2.reserve();
    let mut second = Dict::new();
    second.insert(Name("Second".into()), Object::Int(2));
    update2.set(extra, Object::Dict(second));
    let twice = update2.appended().unwrap();

    let reread = Document::load(twice.clone()).unwrap();
    assert_eq!(
        reread
            .get(ObjRef { num: 1, gen: 0 })
            .unwrap()
            .as_dict()
            .unwrap()
            .get_int("First"),
        Some(1),
        "the first update's change is still visible"
    );
    assert_eq!(
        reread.get(extra).unwrap().as_dict().unwrap().get_int("Second"),
        Some(2),
        "the second update's change is visible"
    );

    let off_a = startxref(&twice).unwrap();
    let info_a = parse_section_at(&twice, off_a).unwrap();
    let off_b = info_a
        .prev
        .expect("the newest section chains to the first update") as usize;
    let info_b = parse_section_at(&twice, off_b).unwrap();
    let off_c = info_b
        .prev
        .expect("the first update chains to the base") as usize;
    let info_c = parse_section_at(&twice, off_c).unwrap();
    assert!(
        info_c.prev.is_none(),
        "three sections in the chain: two updates plus the base"
    );
}

#[test]
fn empty_update_is_refused() {
    let base = classic_base();
    let doc = Document::load(base).unwrap();
    let update = Update::new(&doc).unwrap();
    assert!(matches!(update.appended(), Err(Error::EmptyUpdate)));
}

#[test]
fn encrypted_base_is_refused() {
    let bytes = pdfboss_testkit::encrypted_rc4_doc("secret");
    let doc = Document::load_with_password(bytes, "").unwrap();
    assert!(matches!(Update::new(&doc), Err(Error::EncryptedBase)));
}

/// A hybrid base's newest section, per `startxref`, is its classic table
/// (the `/XRefStm` stream is only ever named from that table's trailer),
/// so the update must append in the classic style even though the merged
/// trailer carries `/Type /XRef` inherited from the hybrid stream.
#[test]
fn hybrid_base_appends_a_classic_table() {
    let bytes = pdfboss_testkit::hybrid_doc();
    let doc = Document::load(bytes).unwrap();
    let base = OverlayBase::from_document(&doc).unwrap();
    assert_eq!(base.kind, XrefStyle::Table);

    let mut update = Update::new(&doc).unwrap();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(3));
    update.set(ObjRef { num: 5, gen: 0 }, Object::Dict(dict));
    let out = update.appended().unwrap();
    let reread = Document::load(out).unwrap();
    assert_eq!(
        reread
            .get(ObjRef { num: 5, gen: 0 })
            .unwrap()
            .as_dict()
            .unwrap()
            .get_int("Marker"),
        Some(3)
    );
}

#[test]
fn append_is_deterministic() {
    let base = classic_base();
    let doc = Document::load(base).unwrap();

    let mut update_a = Update::new(&doc).unwrap();
    let mut dict_a = Dict::new();
    dict_a.insert(Name("Marker".into()), Object::Int(5));
    update_a.set(ObjRef { num: 1, gen: 0 }, Object::Dict(dict_a));
    let a = update_a.appended().unwrap();

    let mut update_b = Update::new(&doc).unwrap();
    let mut dict_b = Dict::new();
    dict_b.insert(Name("Marker".into()), Object::Int(5));
    update_b.set(ObjRef { num: 1, gen: 0 }, Object::Dict(dict_b));
    let b = update_b.appended().unwrap();

    assert_eq!(a, b);
}
