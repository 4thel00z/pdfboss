//! The public update-append API: `OverlayBase`, `Overlay` and `Update` over
//! an existing document, exercised directly rather than through
//! `watermark`.

use pdfboss_core::xref::{parse_section_at, startxref, XrefEntry};
use pdfboss_core::{Dict, Document, Name, ObjRef, Object, XrefKind};
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
    assert!(matches!(update.appended(), Err(Error::EmptyUpdate)));
}

/// A refused update must fail before any byte reaches the destination:
/// `save_appended` on an empty update must not leave a base-only (or
/// otherwise partial) file behind.
#[test]
fn empty_update_save_appended_leaves_no_file() {
    let base = classic_base();
    let doc = Document::load(base).unwrap();
    let update = Update::new(&doc).unwrap();
    let path = std::env::temp_dir().join(format!(
        "pdfboss-update-append-empty-{}.pdf",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    assert!(matches!(
        update.save_appended(&path),
        Err(Error::EmptyUpdate)
    ));
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
    let out = update.appended().unwrap();

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
    let a = update_a.appended().unwrap();

    let mut update_b = Update::new(&doc).unwrap();
    let mut dict_b = Dict::new();
    dict_b.insert(Name("Marker".into()), Object::Int(5));
    update_b.set(ObjRef { num: 1, gen: 0 }, Object::Dict(dict_b));
    let b = update_b.appended().unwrap();

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
    let once = update1.appended().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    update2.remove(marker);
    let out = update2.appended().unwrap();

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
    let once = update1.appended().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    update2.remove(marker);
    let out = update2.appended().unwrap();

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
    let once = update1.appended().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    update2.remove(marker_a);
    update2.remove(marker_b);
    let out = update2.appended().unwrap();

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
    let out = update.appended().unwrap();

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
    let out = update.appended().unwrap();

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
    let once = update1.appended().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    let mut replacement = Dict::new();
    replacement.insert(Name("Marker".into()), Object::Int(2));
    update2.set(marker, Object::Dict(replacement));
    update2.remove(marker);
    let out = update2.appended().unwrap();

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
    let once = update1.appended().unwrap();

    let doc2 = Document::load(once).unwrap();
    let mut update2 = Update::new(&doc2).unwrap();
    update2.remove(marker);
    update2.remove(marker);
    let out = update2.appended().unwrap();

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
    let out = update.appended().unwrap();

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
