//! The public update-append API: `OverlayBase`, `Overlay` and `Update` over
//! an existing document, exercised directly rather than through
//! `watermark`.

use pdfboss_core::xref::{parse_section_at, startxref, XrefEntry};
use pdfboss_core::{Dict, Document, Name, ObjRef, Object, XrefKind};
use pdfboss_write::{
    rotate_pages, Error, Metadata, OverlayBase, Page, PageSize, Pdf, Standard14, Update,
    WriteOptions, Writer, XrefStyle,
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
    let out = update.bytes().unwrap();
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
    let out = update.bytes().unwrap();
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
    let out = update.bytes().unwrap();
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
    let once = update1.bytes().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    let extra = update2.reserve();
    let mut second = Dict::new();
    second.insert(Name("Second".into()), Object::Int(2));
    update2.set(extra, Object::Dict(second));
    let twice = update2.bytes().unwrap();

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
        reread
            .get(extra)
            .unwrap()
            .as_dict()
            .unwrap()
            .get_int("Second"),
        Some(2),
        "the second update's change is visible"
    );

    let off_a = startxref(&twice).unwrap();
    let info_a = parse_section_at(&twice, off_a).unwrap();
    let off_b = info_a
        .prev
        .expect("the newest section chains to the first update") as usize;
    let info_b = parse_section_at(&twice, off_b).unwrap();
    let off_c = info_b.prev.expect("the first update chains to the base") as usize;
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
    assert!(matches!(update.bytes(), Err(Error::EmptyUpdate)));
}

/// A refused update must fail before any byte reaches the destination:
/// `save` on an empty update must not leave a base-only (or
/// otherwise partial) file behind.
#[test]
fn empty_update_save_leaves_no_file() {
    let base = classic_base();
    let doc = Document::load(base).unwrap();
    let update = Update::new(&doc).unwrap();
    let path = std::env::temp_dir().join(format!(
        "pdfboss-update-append-empty-{}.pdf",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    assert!(matches!(update.save(&path), Err(Error::EmptyUpdate)));
    assert!(
        !path.exists(),
        "a refused update must not create the destination file"
    );
}

/// `set` with a caller-chosen number past the base's declared size must
/// raise the next free number past it, so the appended section's own
/// cross-reference stream never collides with it and the section's
/// declared `/Size` still covers it.
#[test]
fn set_past_base_size_advances_next_on_stream_style() {
    let base = stream_base();
    let doc = Document::load(base).unwrap();
    let mut update = Update::new(&doc).unwrap();
    let far = ObjRef {
        num: 10_000,
        gen: 0,
    };
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(11));
    update.set(far, Object::Dict(dict));
    let out = update.bytes().unwrap();

    let reread = Document::load(out.clone()).unwrap();
    assert_eq!(
        reread
            .get(far)
            .unwrap()
            .as_dict()
            .unwrap()
            .get_int("Marker"),
        Some(11),
        "the object set well past the base's size resolves"
    );

    let off = startxref(&out).unwrap();
    let info = parse_section_at(&out, off).unwrap();
    let xref_num = info
        .xref
        .iter()
        .map(|(num, _)| num)
        .max()
        .expect("the appended section carries at least one entry");
    assert_ne!(
        xref_num, far.num,
        "the xref stream got a number distinct from the object set past the base's size"
    );

    let size = reread.xref().trailer.get_int("Size").unwrap();
    assert!(
        size > far.num as i64,
        "the reloaded trailer's /Size ({size}) exceeds the set object's number ({})",
        far.num
    );
}

#[test]
fn encrypted_base_is_refused() {
    let bytes = pdfboss_testkit::encrypted_rc4_doc("secret");
    let doc = Document::load_with_password(bytes, "").unwrap();
    assert!(matches!(Update::new(&doc), Err(Error::EncryptedBase)));
}

/// A literal `/Encrypt null` trailer entry is present but names no
/// dictionary: `Document::load` already treats that as unencrypted (its own
/// predicate is `is_some_and(|o| !o.is_null())`), so `Update::new` must
/// accept the same base rather than refusing it as encrypted.
#[test]
fn encrypt_null_base_is_accepted() {
    let mut builder = pdfboss_testkit::PdfBuilder::new().trailer_extra("/Encrypt null");
    builder.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    builder.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    builder.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
    let bytes = builder.build(1);
    let doc = Document::load(bytes).unwrap();
    assert!(
        Update::new(&doc).is_ok(),
        "a literal /Encrypt null base is degenerate but loadable"
    );
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
    let out = update.bytes().unwrap();

    let out_off = startxref(&out).unwrap();
    assert_eq!(
        parse_section_at(&out, out_off).unwrap().kind,
        XrefKind::Table,
        "the appended section itself is emitted in the classic style"
    );

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
    let a = update_a.bytes().unwrap();

    let mut update_b = Update::new(&doc).unwrap();
    let mut dict_b = Dict::new();
    dict_b.insert(Name("Marker".into()), Object::Int(5));
    update_b.set(ObjRef { num: 1, gen: 0 }, Object::Dict(dict_b));
    let b = update_b.bytes().unwrap();

    assert_eq!(a, b);
}

/// `remove` marks the number free in both xref styles: a reader must not
/// resolve it after reload. The marker is added by a first update (so it
/// exists in the base but nothing else references it) and freed by a
/// second, with two updates chained just as `two_appends_chain` does.
#[test]
fn removed_object_is_gone_after_reload_classic() {
    let base = classic_base();
    let doc1 = Document::load(base).unwrap();
    let mut update1 = Update::new(&doc1).unwrap();
    let marker = update1.reserve();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(1));
    update1.set(marker, Object::Dict(dict));
    let once = update1.bytes().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    update2.remove(marker);
    let out = update2.bytes().unwrap();

    let reread = Document::load(out).unwrap();
    assert!(matches!(
        reread.get(marker),
        Err(pdfboss_core::Error::ObjectNotFound(..))
    ));
}

#[test]
fn removed_object_is_gone_after_reload_stream() {
    let base = stream_base();
    let doc1 = Document::load(base).unwrap();
    let mut update1 = Update::new(&doc1).unwrap();
    let marker = update1.reserve();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(1));
    update1.set(marker, Object::Dict(dict));
    let once = update1.bytes().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    update2.remove(marker);
    let out = update2.bytes().unwrap();

    let reread = Document::load(out).unwrap();
    assert!(matches!(
        reread.get(marker),
        Err(pdfboss_core::Error::ObjectNotFound(..))
    ));
}

/// The classic table's free chain always starts at entry 0: its row names
/// the lowest freed number as the chain's head, so a reader following the
/// chain from object 0 reaches the freed object first. Freeing two objects
/// pins both ends of the chain: the head row names the lower number, that
/// number's own row names the higher one, and the higher number's row
/// closes the chain back to 0.
#[test]
fn free_chain_starts_at_entry_zero() {
    let base = classic_base();
    let doc1 = Document::load(base).unwrap();
    let mut update1 = Update::new(&doc1).unwrap();
    let marker_a = update1.reserve();
    let marker_b = update1.reserve();
    let mut dict_a = Dict::new();
    dict_a.insert(Name("Marker".into()), Object::Int(1));
    update1.set(marker_a, Object::Dict(dict_a));
    let mut dict_b = Dict::new();
    dict_b.insert(Name("Marker".into()), Object::Int(2));
    update1.set(marker_b, Object::Dict(dict_b));
    let once = update1.bytes().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    update2.remove(marker_a);
    update2.remove(marker_b);
    let out = update2.bytes().unwrap();

    let off = startxref(&out).unwrap();
    let info = parse_section_at(&out, off).unwrap();
    assert_eq!(
        info.xref.get(marker_a.num),
        Some(XrefEntry::Free),
        "the lower freed number's own entry reads back as free"
    );
    assert_eq!(
        info.xref.get(marker_b.num),
        Some(XrefEntry::Free),
        "the higher freed number's own entry reads back as free"
    );

    let text = String::from_utf8_lossy(&out);
    let head_row = format!("{:010} 65535 f \n", marker_a.num);
    assert!(
        text.contains(&head_row),
        "the entry-0 subsection row names the lower freed number as the chain's head: {text}"
    );
    let middle_row = format!("{:010} 00001 f \n", marker_b.num);
    assert!(
        text.contains(&middle_row),
        "the lower freed number's own row names the higher one next: {text}"
    );
    let tail_row = "0000000000 00001 f \n";
    assert!(
        text.contains(tail_row),
        "the higher freed number's own row closes the chain back to 0: {text}"
    );
}

/// The base's own `/ID` half is copied verbatim into the appended trailer;
/// the second half is replaced, not copied, so it differs from the base's.
#[test]
fn id_first_half_survives_second_rotates() {
    let base = classic_base();
    let doc = Document::load(base).unwrap();
    let base_id = doc
        .xref()
        .trailer
        .get_array("ID")
        .expect("the base carries an /ID array")
        .to_vec();
    let base_first = base_id[0]
        .as_str_bytes()
        .expect("the first half is a string")
        .to_vec();
    let base_second = base_id[1]
        .as_str_bytes()
        .expect("the second half is a string")
        .to_vec();

    let mut update = Update::new(&doc).unwrap();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(1));
    update.set(ObjRef { num: 1, gen: 0 }, Object::Dict(dict));
    let out = update.bytes().unwrap();

    let reread = Document::load(out).unwrap();
    let id = reread
        .xref()
        .trailer
        .get_array("ID")
        .expect("the appended trailer carries an /ID array");
    let first = id[0].as_str_bytes().expect("the first half is a string");
    let second = id[1].as_str_bytes().expect("the second half is a string");
    assert_eq!(
        first,
        base_first.as_slice(),
        "the first half survives untouched"
    );
    assert_ne!(second, base_second.as_slice(), "the second half rotates");
}

/// Two frees-only updates of the same base, freeing different objects, must
/// rotate `/ID`'s second half differently: the digest folds each freed
/// `(num, gen)` pair in, so a section with no set bodies at all (an empty
/// `body`) still changes what it frees into a distinct second half. Running
/// the same free twice over the same base stays deterministic.
#[test]
fn id_second_half_folds_freed_pairs() {
    let base = classic_base();
    let doc = Document::load(base).unwrap();

    let mut seed = Update::new(&doc).unwrap();
    let marker_a = seed.reserve();
    let marker_b = seed.reserve();
    let mut dict_a = Dict::new();
    dict_a.insert(Name("Marker".into()), Object::Int(1));
    seed.set(marker_a, Object::Dict(dict_a));
    let mut dict_b = Dict::new();
    dict_b.insert(Name("Marker".into()), Object::Int(2));
    seed.set(marker_b, Object::Dict(dict_b));
    let seeded = seed.bytes().unwrap();
    let seeded_doc = Document::load(seeded).unwrap();

    fn second_half(bytes: Vec<u8>) -> Vec<u8> {
        let reread = Document::load(bytes).unwrap();
        let id = reread
            .xref()
            .trailer
            .get_array("ID")
            .expect("the appended trailer carries an /ID array")
            .to_vec();
        id[1].as_str_bytes().unwrap().to_vec()
    }

    let mut update_a = Update::new(&seeded_doc).unwrap();
    update_a.remove(marker_a);
    let out_a = update_a.bytes().unwrap();

    let mut update_b = Update::new(&seeded_doc).unwrap();
    update_b.remove(marker_b);
    let out_b = update_b.bytes().unwrap();

    assert_ne!(
        second_half(out_a.clone()),
        second_half(out_b),
        "freeing different objects rotates /ID's second half differently"
    );

    let mut update_a2 = Update::new(&seeded_doc).unwrap();
    update_a2.remove(marker_a);
    let out_a2 = update_a2.bytes().unwrap();
    assert_eq!(
        second_half(out_a),
        second_half(out_a2),
        "the same free, run twice over the same base, rotates /ID identically"
    );
}

/// `/Index` groups sorted rows into maximal contiguous runs rather than one
/// pair per object: objects 1-3 form one run, an isolated replacement and
/// the xref stream's own row each stay isolated. The assertion inspects the
/// section bytes directly, since resolving all four objects after reload
/// would pass even without coalescing.
#[test]
fn index_pairs_coalesce_runs() {
    let base = stream_base();
    let doc = Document::load(base).unwrap();
    let base_size = OverlayBase::from_document(&doc).unwrap().size;
    let mut update = Update::new(&doc).unwrap();
    for num in [1u32, 2, 3, 6] {
        let mut dict = Dict::new();
        dict.insert(Name("Marker".into()), Object::Int(i64::from(num)));
        update.set(ObjRef { num, gen: 0 }, Object::Dict(dict));
    }
    let out = update.bytes().unwrap();

    let reread = Document::load(out.clone()).unwrap();
    for num in [1u32, 2, 3, 6] {
        assert!(
            reread.get(ObjRef { num, gen: 0 }).is_ok(),
            "object {num} resolves after reload"
        );
    }

    let off = startxref(&out).unwrap();
    let section = &out[off..];
    let text = String::from_utf8_lossy(section);
    let index_at = text.find("/Index").expect("the xref stream names /Index");
    let array_start = text[index_at..].find('[').unwrap() + index_at;
    let array_end = text[array_start..].find(']').unwrap() + array_start;
    let numbers: Vec<i64> = text[array_start + 1..array_end]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    assert_eq!(
        numbers,
        vec![1, 3, 6, 1, i64::from(base_size), 1],
        "the run 1..4 coalesces into one pair; 6 and the xref stream's own row stay isolated"
    );
}

/// A number recorded twice in one update, once by `set` and once by
/// `remove`, keeps only its last-recorded change: the appended section
/// carries exactly one row for that number (the free one), not one row
/// per recorded change.
#[test]
fn set_then_remove_same_ref_emits_one_free_row() {
    let base = classic_base();
    let doc1 = Document::load(base).unwrap();
    let mut update1 = Update::new(&doc1).unwrap();
    let marker = update1.reserve();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(1));
    update1.set(marker, Object::Dict(dict));
    let once = update1.bytes().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    let mut replacement = Dict::new();
    replacement.insert(Name("Marker".into()), Object::Int(2));
    update2.set(marker, Object::Dict(replacement));
    update2.remove(marker);
    let out = update2.bytes().unwrap();

    let reread = Document::load(out.clone()).unwrap();
    assert!(matches!(
        reread.get(marker),
        Err(pdfboss_core::Error::ObjectNotFound(..))
    ));

    let off = startxref(&out).unwrap();
    let text = String::from_utf8_lossy(&out[off..]);
    let header = format!("\n{} 1\n", marker.num);
    assert_eq!(
        text.matches(&header).count(),
        1,
        "exactly one subsection row, in this update's own section, for the number set then removed: {text}"
    );
}

/// `set` of object number 0 is a documented no-op, symmetric with
/// `remove`'s existing guard: object 0 is the free-list head, already
/// represented by the section's synthetic entry-0 row whenever anything
/// else is freed, so a `set` row for it must never also appear.
#[test]
fn set_object_zero_is_a_no_op() {
    let base = classic_base();
    let doc1 = Document::load(base).unwrap();
    let mut update1 = Update::new(&doc1).unwrap();
    let marker = update1.reserve();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(1));
    update1.set(marker, Object::Dict(dict));
    let once = update1.bytes().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    let mut zero_dict = Dict::new();
    zero_dict.insert(Name("Marker".into()), Object::Int(99));
    update2.set(ObjRef { num: 0, gen: 0 }, Object::Dict(zero_dict));
    update2.remove(marker);
    let out = update2.bytes().unwrap();

    let off = startxref(&out).unwrap();
    let text = String::from_utf8_lossy(&out[off..]);
    let header = "\n0 1\n";
    assert_eq!(
        text.matches(header).count(),
        1,
        "exactly one subsection row for object number 0: {text}"
    );

    let reread = Document::load(out).unwrap();
    assert!(matches!(
        reread.get(marker),
        Err(pdfboss_core::Error::ObjectNotFound(..))
    ));
}

/// Calling `remove` twice on the same reference records only one free row,
/// not two entries chasing the same number through the chain.
#[test]
fn remove_twice_emits_one_free_row() {
    let base = classic_base();
    let doc1 = Document::load(base).unwrap();
    let mut update1 = Update::new(&doc1).unwrap();
    let marker = update1.reserve();
    let mut dict = Dict::new();
    dict.insert(Name("Marker".into()), Object::Int(1));
    update1.set(marker, Object::Dict(dict));
    let once = update1.bytes().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    update2.remove(marker);
    update2.remove(marker);
    let out = update2.bytes().unwrap();

    let reread = Document::load(out.clone()).unwrap();
    assert!(matches!(
        reread.get(marker),
        Err(pdfboss_core::Error::ObjectNotFound(..))
    ));

    let off = startxref(&out).unwrap();
    let text = String::from_utf8_lossy(&out[off..]);
    let header = format!("\n{} 1\n", marker.num);
    assert_eq!(
        text.matches(&header).count(),
        1,
        "exactly one subsection row, in this update's own section, for a number removed twice: {text}"
    );
}

/// A number recorded more than once (two `set`s, then a `remove`) must
/// still leave the xref stream's `/Index` non-overlapping: each object
/// number appears in exactly one run, never split across two.
#[test]
fn index_stays_non_overlapping_with_duplicate_changes() {
    let base = stream_base();
    let doc = Document::load(base).unwrap();
    let mut update = Update::new(&doc).unwrap();
    let target = ObjRef { num: 5, gen: 0 };
    let mut first = Dict::new();
    first.insert(Name("Marker".into()), Object::Int(1));
    update.set(target, Object::Dict(first));
    let mut second = Dict::new();
    second.insert(Name("Marker".into()), Object::Int(2));
    update.set(target, Object::Dict(second));
    update.remove(target);
    let out = update.bytes().unwrap();

    let reread = Document::load(out.clone()).unwrap();
    assert!(matches!(
        reread.get(target),
        Err(pdfboss_core::Error::ObjectNotFound(..))
    ));

    let off = startxref(&out).unwrap();
    let section = &out[off..];
    let text = String::from_utf8_lossy(section);
    let index_at = text.find("/Index").expect("the xref stream names /Index");
    let array_start = text[index_at..].find('[').unwrap() + index_at;
    let array_end = text[array_start..].find(']').unwrap() + array_start;
    let numbers: Vec<i64> = text[array_start + 1..array_end]
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    let mut seen = std::collections::HashSet::new();
    for pair in numbers.chunks(2) {
        let (run_start, count) = (pair[0], pair[1]);
        for object_num in run_start..run_start + count {
            assert!(
                seen.insert(object_num),
                "object {object_num} appears in more than one /Index run: {numbers:?}"
            );
        }
    }
}

fn base_pdf_with_metadata(xref: XrefStyle, meta: Metadata) -> Vec<u8> {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("Base page", 72.0, 700.0, Standard14::Helvetica, 14.0)
        .unwrap();
    Pdf {
        pages: vec![page],
        metadata: Some(meta),
        options: WriteOptions {
            xref,
            ..WriteOptions::default()
        },
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap()
}

/// Setting only `title` on a base that already carries `title` and `author`
/// must keep `author` untouched: a `None` field never clears an existing
/// key, only a `Some` field overwrites one.
#[test]
fn set_metadata_merges_existing_fields() {
    let base = base_pdf_with_metadata(
        XrefStyle::Table,
        Metadata {
            title: Some("Old".to_string()),
            author: Some("Keep".to_string()),
            ..Metadata::default()
        },
    );
    let doc = Document::load(base).unwrap();
    let mut update = Update::new(&doc).unwrap();
    update
        .set_metadata(Metadata {
            title: Some("New".to_string()),
            ..Metadata::default()
        })
        .unwrap();
    let out = update.bytes().unwrap();

    let reread = Document::load(out).unwrap();
    let meta = reread.metadata();
    assert_eq!(meta.title.as_deref(), Some("New"));
    assert_eq!(meta.author.as_deref(), Some("Keep"));
}

/// A base with no `/Info` at all still gets one on `set_metadata`: the
/// merge target is a freshly reserved object rather than an existing ref.
#[test]
fn set_metadata_creates_info_when_absent() {
    let base = classic_base();
    let doc = Document::load(base).unwrap();
    let mut update = Update::new(&doc).unwrap();
    update
        .set_metadata(Metadata {
            title: Some("Fresh".to_string()),
            ..Metadata::default()
        })
        .unwrap();
    let out = update.bytes().unwrap();

    let reread = Document::load(out).unwrap();
    assert_eq!(reread.metadata().title.as_deref(), Some("Fresh"));
}

/// A base whose catalog already carries `/Metadata` (every `Pdf` with
/// metadata writes one) gets that packet rewritten from the merged fields:
/// the reloaded stream carries the new title, not the old.
#[test]
fn set_metadata_rewrites_xmp_when_catalog_has_it() {
    let base = base_pdf_with_metadata(
        XrefStyle::Table,
        Metadata {
            title: Some("Old".to_string()),
            ..Metadata::default()
        },
    );
    let doc = Document::load(base).unwrap();
    let mut update = Update::new(&doc).unwrap();
    update
        .set_metadata(Metadata {
            title: Some("New".to_string()),
            ..Metadata::default()
        })
        .unwrap();
    let out = update.bytes().unwrap();

    let reread = Document::load(out).unwrap();
    let root = reread.xref().trailer.get_ref("Root").unwrap();
    let catalog = reread.get(root).unwrap();
    let metadata_ref = catalog.as_dict().unwrap().get_ref("Metadata").unwrap();
    let stream = reread.get(metadata_ref).unwrap();
    let text = String::from_utf8(stream.as_stream().unwrap().data.clone()).unwrap();
    assert!(text.contains("New"));
    assert!(!text.contains("Old"));
}

/// A base built directly through `Writer` (not `Pdf`, which always writes
/// `/Info` fields as direct strings) whose `/Info /Title` is an indirect
/// reference to its own string object, and whose catalog carries an XMP
/// packet.
fn writer_base_with_indirect_title() -> Vec<u8> {
    let mut w = Writer::new(WriteOptions {
        xref: XrefStyle::Table,
        ..WriteOptions::default()
    });
    let pages_root = w.reserve();
    let page = w.reserve();

    let mut page_dict = Dict::new();
    page_dict.insert(Name("Type".into()), Object::Name(Name("Page".into())));
    page_dict.insert(Name("Parent".into()), Object::Ref(pages_root));
    page_dict.insert(Name("Resources".into()), Object::Dict(Dict::new()));
    page_dict.insert(
        Name("MediaBox".into()),
        Object::Array(vec![
            Object::Int(0),
            Object::Int(0),
            Object::Int(612),
            Object::Int(792),
        ]),
    );
    w.fill(page, Object::Dict(page_dict)).unwrap();

    let mut pages_dict = Dict::new();
    pages_dict.insert(Name("Type".into()), Object::Name(Name("Pages".into())));
    pages_dict.insert(Name("Kids".into()), Object::Array(vec![Object::Ref(page)]));
    pages_dict.insert(Name("Count".into()), Object::Int(1));
    w.fill(pages_root, Object::Dict(pages_dict)).unwrap();

    let title_ref = w.put(Object::String(b"Indirect Title".to_vec()));
    let mut info = Dict::new();
    info.insert(Name("Title".into()), Object::Ref(title_ref));
    let info_ref = w.put(Object::Dict(info));
    w.set_info(info_ref);

    let mut xmp_dict = Dict::new();
    xmp_dict.insert(Name("Type".into()), Object::Name(Name("Metadata".into())));
    xmp_dict.insert(Name("Subtype".into()), Object::Name(Name("XML".into())));
    let xmp_ref = w.put_stream_raw(xmp_dict, b"<x:xmpmeta></x:xmpmeta>".to_vec());

    let mut catalog = Dict::new();
    catalog.insert(Name("Type".into()), Object::Name(Name("Catalog".into())));
    catalog.insert(Name("Pages".into()), Object::Ref(pages_root));
    catalog.insert(Name("Metadata".into()), Object::Ref(xmp_ref));
    let root = w.put(Object::Dict(catalog));

    w.finish(root).unwrap()
}

/// An `/Info /Title` stored as an indirect reference must still reach the
/// rewritten XMP packet: `set_metadata` only touches `author`, so `Title`
/// is a kept (`None`) field, and a kept indirect string must resolve
/// rather than silently drop out of the merged `Metadata`.
#[test]
fn set_metadata_resolves_indirect_info_values_into_xmp() {
    let base = writer_base_with_indirect_title();
    let doc = Document::load(base).unwrap();
    let mut update = Update::new(&doc).unwrap();
    update
        .set_metadata(Metadata {
            author: Some("New Author".to_string()),
            ..Metadata::default()
        })
        .unwrap();
    let out = update.bytes().unwrap();

    let reread = Document::load(out).unwrap();
    let root = reread.xref().trailer.get_ref("Root").unwrap();
    let catalog = reread.get(root).unwrap();
    let metadata_ref = catalog.as_dict().unwrap().get_ref("Metadata").unwrap();
    let stream = reread.get(metadata_ref).unwrap();
    let text = String::from_utf8(stream.as_stream().unwrap().data.clone()).unwrap();
    assert!(
        text.contains("Indirect Title"),
        "a kept indirect /Info value must still reach the rewritten XMP packet: {text}"
    );
}

/// Rotating pages 1 and 3 of a three-page document by 90 degrees clockwise
/// stages each page's own object with its effective rotation plus 90,
/// leaving the untouched page at 0. The base bytes stay in place at the
/// front of the output, since this is an incremental update.
#[test]
fn rotate_pages_marks_selected_pages_and_keeps_the_prefix() {
    let base = pdfboss_testkit::multi_page_doc(&["one", "two", "three"]);
    let doc = Document::load(base.clone()).unwrap();
    let mut update = Update::new(&doc).unwrap();
    rotate_pages(&mut update, &[0, 2], 90).unwrap();
    let out = update.bytes().unwrap();
    assert_eq!(
        &out[..base.len()],
        &base[..],
        "an update keeps the base bytes in place"
    );

    let reread = Document::load(out).unwrap();
    for (index, expected) in [90, 0, 90].iter().enumerate() {
        let page = reread.page(index).unwrap();
        assert_eq!(page.rotate, *expected, "page {index}");
    }
}

/// A page inlined directly into `/Kids`, with no object of its own, cannot
/// be staged as a replacement object: `rotate_pages` refuses it, naming
/// its 1-based page number and pointing at `--rewrite`.
#[test]
fn rotate_pages_refuses_an_inline_page() {
    let mut b = pdfboss_testkit::PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(
        2,
        "<< /Type /Pages /Count 1 /Kids [ << /Type /Page /Parent 2 0 R \
         /MediaBox [0 0 612 792] >> ] >>",
    );
    let base = b.build(1);
    let doc = Document::load(base).unwrap();
    let mut update = Update::new(&doc).unwrap();

    let result = rotate_pages(&mut update, &[0], 90);
    let Err(Error::Other(message)) = result else {
        panic!("expected Error::Other, got {result:?}");
    };
    assert!(message.contains("page 1"), "message: {message}");
    assert!(message.contains("--rewrite"), "message: {message}");
}
