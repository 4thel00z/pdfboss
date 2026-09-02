//! Encrypted emission through the `Writer`: `new_encrypted`, the
//! `/Encrypt` object and trailer entry, and each exemption the spec
//! requires — the `/Encrypt` dictionary itself, the trailer's `/ID`
//! strings, the `/Type /XRef` cross-reference stream, and object-stream
//! member strings (encrypted only at the container level).
//!
//! `Document::load_with_password` is the oracle: every round trip here
//! opens through it with the user or owner password, fails without one,
//! and fails with a wrong one.

use pdfboss_core::parser::{NoResolve, Parser};
use pdfboss_core::xref::{load_xref, XrefEntry};
use pdfboss_core::{Dict, Document, Encryptor, Error, Name, ObjRef, Object, Permissions};
use pdfboss_output::{extract_text, ReadingOrder};
use pdfboss_write::{WriteOptions, Writer, XrefStyle};

const CONTENT: &[u8] = b"BT /F1 12 Tf 72 720 Td (Hello, encrypted) Tj ET";
const USER_PW: &str = "user-pw";
const OWNER_PW: &str = "owner-pw";

fn name(text: &str) -> Name {
    Name(text.to_string())
}

/// A deterministic byte source: increasing bytes, never the operating
/// system's randomness, so ciphertext and key material stay stable across
/// runs and assertions can compare exact bytes.
#[allow(
    clippy::type_complexity,
    reason = "Box<dyn FnMut(&mut [u8]) + Send> matches Encryptor::aes256_with_rng's parameter"
)]
fn counter_rng() -> Box<dyn FnMut(&mut [u8]) + Send> {
    let mut c = 0u8;
    Box::new(move |b: &mut [u8]| {
        for x in b {
            c = c.wrapping_add(1);
            *x = c;
        }
    })
}

fn encryptor() -> (Encryptor, Dict) {
    Encryptor::aes256_with_rng(USER_PW, OWNER_PW, Permissions::all(), counter_rng())
}

fn page_dict(pages: ObjRef, content: ObjRef) -> Dict {
    let mut page = Dict::new();
    page.insert(name("Type"), Object::Name(name("Page")));
    page.insert(name("Parent"), Object::Ref(pages));
    page.insert(
        name("MediaBox"),
        Object::Array(vec![
            Object::Int(0),
            Object::Int(0),
            Object::Int(612),
            Object::Int(792),
        ]),
    );
    page.insert(name("Contents"), Object::Ref(content));
    page
}

/// The minimal catalog/pages/page/content graph `crates/pdfboss-write/src/
/// writer.rs`'s own unit tests build, by hand, against a caller-supplied
/// writer (encrypted or not). Returns the catalog's reference, the root
/// `finish` needs.
fn build(w: &mut Writer) -> ObjRef {
    let content = w.put_stream(Dict::new(), CONTENT.to_vec());
    let pages = w.reserve();
    let page = w.put(Object::Dict(page_dict(pages, content)));
    let mut tree = Dict::new();
    tree.insert(name("Type"), Object::Name(name("Pages")));
    tree.insert(name("Kids"), Object::Array(vec![Object::Ref(page)]));
    tree.insert(name("Count"), Object::Int(1));
    w.fill(pages, Object::Dict(tree))
        .expect("pages slot is fillable");
    let mut catalog = Dict::new();
    catalog.insert(name("Type"), Object::Name(name("Catalog")));
    catalog.insert(name("Pages"), Object::Ref(pages));
    w.put(Object::Dict(catalog))
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// Parses one indirect object directly out of raw file bytes, with no
/// decryption applied at all — the same low-level parse
/// `Document::load_with_password` itself uses to read `/Encrypt` before a
/// decryptor exists. Only valid for an object with no nested indirect
/// references (`NoResolve` never resolves any).
fn parse_raw_object(bytes: &[u8], r: ObjRef) -> Object {
    let xref = load_xref(bytes).expect("xref loads without a password");
    let offset = match xref.get(r.num) {
        Some(XrefEntry::InFile { offset, .. }) => offset as usize,
        other => panic!("expected an in-file entry for {r:?}, got {other:?}"),
    };
    let (_, obj) = Parser::at(bytes, offset)
        .parse_indirect(&NoResolve)
        .expect("object parses");
    obj
}

#[test]
fn encrypted_writer_round_trips_with_the_user_password() {
    let (encryptor, dict) = encryptor();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let bytes = w.finish(root).expect("encrypted document finishes");

    assert!(
        matches!(Document::load(bytes.clone()), Err(Error::Encrypted)),
        "no password must not open an encrypted file"
    );

    let doc = Document::load_with_password(bytes, USER_PW).expect("user password opens");
    assert_eq!(doc.page_count(), 1);
    let page = doc.page(0).expect("page 0 exists");
    let text = extract_text(&doc, &page, ReadingOrder::Content).expect("text extracts");
    assert!(text.contains("Hello, encrypted"), "{text:?}");
}

#[test]
fn owner_password_also_opens() {
    let (encryptor, dict) = encryptor();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let bytes = w.finish(root).expect("encrypted document finishes");

    let doc = Document::load_with_password(bytes, OWNER_PW).expect("owner password opens");
    assert_eq!(doc.page_count(), 1);
}

#[test]
fn wrong_password_fails_to_open() {
    let (encryptor, dict) = encryptor();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let bytes = w.finish(root).expect("encrypted document finishes");

    assert!(matches!(
        Document::load_with_password(bytes, "not-it"),
        Err(Error::Encrypted)
    ));
}

/// Exemption: the `/Encrypt` dictionary itself is never encrypted. Its
/// `/U`, `/UE`, `/O` and `/OE` strings must equal the dict handed to
/// `new_encrypted`, verified from a raw, undecrypted parse — running them
/// back through the normal decrypting read path would treat already-plain
/// bytes as ciphertext and corrupt them, which is exactly what emitting
/// this object unencrypted must avoid.
#[test]
fn encrypt_dict_strings_are_never_encrypted() {
    let (encryptor, dict) = encryptor();
    let expected = dict.clone();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let bytes = w.finish(root).expect("encrypted document finishes");

    let xref = load_xref(&bytes).expect("xref loads without a password");
    let encrypt_ref = xref
        .trailer
        .get_ref("Encrypt")
        .expect("trailer carries /Encrypt as a reference");
    let actual = parse_raw_object(&bytes, encrypt_ref);
    let actual = actual.as_dict().expect("/Encrypt is a dictionary");

    for key in ["U", "UE", "O", "OE"] {
        assert_eq!(
            actual.get(key).and_then(Object::as_str_bytes),
            expected.get(key).and_then(Object::as_str_bytes),
            "/{key} must round-trip unencrypted"
        );
    }

    Document::load_with_password(bytes.clone(), USER_PW).expect("user password opens");
    Document::load_with_password(bytes, OWNER_PW).expect("owner password opens");
}

/// Exemption: the trailer's `/ID` strings never pass through
/// `write_indirect`, so they are never encrypted. A raw, undecrypted parse
/// of the trailer must show a plausible 16-byte pair, identical to what a
/// password-authenticated `Document` reports.
#[test]
fn id_pair_is_never_encrypted() {
    let (encryptor, dict) = encryptor();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let bytes = w.finish(root).expect("encrypted document finishes");

    let raw_id = load_xref(&bytes)
        .expect("xref loads without a password")
        .trailer
        .get_array("ID")
        .expect("/ID array present")
        .to_vec();
    assert_eq!(raw_id.len(), 2);
    let raw_first = raw_id[0]
        .as_str_bytes()
        .expect("/ID entry is a string")
        .to_vec();
    assert_eq!(
        raw_first.len(),
        16,
        "a SHA-256-derived /ID half is 16 bytes"
    );
    assert_eq!(raw_id[0], raw_id[1], "both /ID halves are identical");

    let doc = Document::load_with_password(bytes, USER_PW).expect("user password opens");
    let doc_id = doc
        .xref()
        .trailer
        .get_array("ID")
        .expect("/ID array present");
    assert_eq!(
        doc_id[0].as_str_bytes().expect("/ID entry is a string"),
        &raw_first[..],
        "/ID must be identical whether read raw or through a password load"
    );
}

/// Exemption: the `/Type /XRef` cross-reference stream is never encrypted.
/// If it had been, this direct parse (the same one
/// `Document::load_with_password` performs before any decryptor is
/// configured) would fail outright, since the xref locates every other
/// object in the file.
#[test]
fn xref_stream_is_never_encrypted() {
    let (encryptor, dict) = encryptor();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let bytes = w.finish(root).expect("encrypted document finishes");

    let xref = load_xref(&bytes).expect("xref stream parses without a password");
    assert!(xref.trailer.get("Encrypt").is_some());

    let doc = Document::load_with_password(bytes, USER_PW).expect("user password opens");
    assert_eq!(doc.page_count(), 1);
}

/// Exemption: object-stream members serialize in plaintext into the
/// container; the container itself is encrypted as one stream. A packed
/// dictionary's string must come back correct after a password load,
/// exercising container-level (not member-level) encryption.
#[test]
fn object_stream_members_are_readable_after_a_password_load() {
    let (encryptor, dict) = encryptor();
    let options = WriteOptions {
        xref: XrefStyle::Stream,
        compress: true,
        object_streams: true,
        version: (1, 7),
    };
    let mut w = Writer::new_encrypted(options, encryptor, dict);
    let root = build(&mut w);
    let mut marker = Dict::new();
    marker.insert(
        name("Marker"),
        Object::String(b"packed member text".to_vec()),
    );
    let marker_ref = w.put(Object::Dict(marker));
    let bytes = w.finish(root).expect("encrypted document finishes");

    assert!(
        count_occurrences(&bytes, b"/ObjStm") >= 1,
        "object streams must actually be used"
    );

    let doc = Document::load_with_password(bytes, USER_PW).expect("user password opens");
    let resolved = doc
        .resolve(&Object::Ref(marker_ref))
        .expect("packed object resolves");
    let marker_dict = resolved.as_dict().expect("packed object is a dictionary");
    assert_eq!(
        marker_dict.get("Marker").and_then(Object::as_str_bytes),
        Some(&b"packed member text"[..])
    );
}

/// The same document round-trips under the classic `xref` table flavor,
/// which has no object streams and no cross-reference stream to exempt —
/// only the `/Encrypt` dictionary and the `/ID` strings.
#[test]
fn table_mode_round_trips_encrypted() {
    let (encryptor, dict) = encryptor();
    let options = WriteOptions {
        xref: XrefStyle::Table,
        compress: false,
        object_streams: false,
        version: (1, 7),
    };
    let mut w = Writer::new_encrypted(options, encryptor, dict);
    let root = build(&mut w);
    let bytes = w.finish(root).expect("encrypted document finishes");

    assert!(
        matches!(Document::load(bytes.clone()), Err(Error::Encrypted)),
        "no password must not open an encrypted file"
    );
    assert!(matches!(
        Document::load_with_password(bytes.clone(), "not-it"),
        Err(Error::Encrypted)
    ));

    let doc = Document::load_with_password(bytes, USER_PW).expect("user password opens");
    assert_eq!(doc.page_count(), 1);
    let page = doc.page(0).expect("page 0 exists");
    let text = extract_text(&doc, &page, ReadingOrder::Content).expect("text extracts");
    assert!(text.contains("Hello, encrypted"), "{text:?}");
}
