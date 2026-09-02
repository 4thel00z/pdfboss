//! Encrypted emission through the `Writer`: `new_encrypted`, the
//! `/Encrypt` object and trailer entry, and each exemption the spec
//! requires: the `/Encrypt` dictionary itself, the trailer's `/ID`
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
use pdfboss_testkit::PdfBuilder;
use pdfboss_write::{decrypt_document, encrypt_document, rewrite_document, WriteOptions, Writer, XrefStyle};

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
/// decryption applied at all: the same low-level parse
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

fn hexstr(b: &[u8]) -> String {
    let mut s = String::from("<");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s.push('>');
    s
}

/// A hand-built `/Encrypt` dict body for a source PDF assembled directly
/// through `PdfBuilder`, not through this crate's own `Writer`: the write
/// side never emits `/EncryptMetadata false` (`Encryptor::aes256_with_rng`
/// always leaves it absent, meaning true), so a fixture with it set has to
/// be assembled by hand, the way a third-party writer like Acrobat would.
fn encrypt_dict_body(dict: &Dict) -> String {
    let field = |key: &str| hexstr(dict.get(key).and_then(Object::as_str_bytes).unwrap());
    format!(
        "<< /Filter /Standard /V 5 /R 6 /Length 256 /P {} /U {} /UE {} /O {} /OE {} /Perms {} \
         /EncryptMetadata false /CF << /StdCF << /CFM /AESV3 /Length 32 >> >> \
         /StmF /StdCF /StrF /StdCF >>",
        dict.get_int("P").unwrap(),
        field("U"),
        field("UE"),
        field("O"),
        field("OE"),
        field("Perms"),
    )
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
/// `new_encrypted`, verified from a raw, undecrypted parse. Running them
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
/// which has no object streams and no cross-reference stream to exempt:
/// only the `/Encrypt` dictionary and the `/ID` strings apply.
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

/// A locked `Document` (encrypted, no working decryptor) cannot come into
/// being through the public sync load path: a wrong or missing password
/// fails inside `load_with_password` itself, before any `Document` value
/// exists. This pins that load-time refusal and its exact message.
/// `Importer::new`'s own `is_locked` refusal, and so `encrypt_document`'s
/// and `decrypt_document`'s, is a second safeguard behind it that this
/// crate's public API can never actually reach on its own.
#[test]
fn loading_an_encrypted_fixture_without_a_password_fails_at_load() {
    let (encryptor, dict) = encryptor();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let bytes = w.finish(root).expect("encrypted document finishes");

    let Err(err) = Document::load(bytes) else {
        panic!("no password must not open an encrypted file");
    };
    assert_eq!(err.to_string(), "encrypted documents are not supported");
}

/// A plain, unencrypted document encrypts under both a user and an owner
/// password: each opens the result, a wrong password does not, and the
/// text extracted from the encrypted output still matches the original.
#[test]
fn encrypt_document_round_trips_with_the_user_and_owner_password() {
    let mut w = Writer::new(WriteOptions::default());
    let root = build(&mut w);
    let plain_bytes = w.finish(root).expect("plain document finishes");
    let plain = Document::load(plain_bytes).expect("plain document loads");

    let bytes = encrypt_document(
        &plain,
        USER_PW,
        OWNER_PW,
        Permissions::all(),
        WriteOptions::default(),
    )
    .expect("encrypt_document succeeds");

    assert!(
        matches!(Document::load(bytes.clone()), Err(Error::Encrypted)),
        "no password must not open the encrypted output"
    );
    assert!(matches!(
        Document::load_with_password(bytes.clone(), "not-it"),
        Err(Error::Encrypted)
    ));

    for password in [USER_PW, OWNER_PW] {
        let doc = Document::load_with_password(bytes.clone(), password)
            .unwrap_or_else(|err| panic!("password {password:?} opens: {err}"));
        assert_eq!(doc.page_count(), 1);
        let page = doc.page(0).expect("page 0 exists");
        let text = extract_text(&doc, &page, ReadingOrder::Content).expect("text extracts");
        assert!(text.contains("Hello, encrypted"), "{text:?}");
    }
}

/// An empty `owner_password` falls back to `user_password`: `/O` and `/OE`
/// end up derived from `"user-pw"` too, so it opens the file whether a
/// reader tries it as the user or the owner password (the two checks are
/// indistinguishable here since both stored passwords are now the same
/// string). Critically, `Document::load` with NO password at all must
/// still fail: a regression that let the fallback lapse (leaving `/O`
/// derived from the literal empty string) would make the empty password
/// itself open the file, since a no-password load tries `""` against both
/// the user and the recovered owner check.
#[test]
fn encrypt_document_falls_back_to_the_user_password_for_an_empty_owner_password() {
    let mut w = Writer::new(WriteOptions::default());
    let root = build(&mut w);
    let plain_bytes = w.finish(root).expect("plain document finishes");
    let plain = Document::load(plain_bytes).expect("plain document loads");

    let bytes = encrypt_document(
        &plain,
        "user-pw",
        "",
        Permissions::all(),
        WriteOptions::default(),
    )
    .expect("an empty owner password falls back to the user password");

    let doc = Document::load_with_password(bytes.clone(), "user-pw")
        .expect("user-pw opens, whether matched as the user or the fallen-back owner password");
    assert_eq!(doc.page_count(), 1);

    assert!(
        matches!(Document::load(bytes), Err(Error::Encrypted)),
        "no password must not open the file: an un-fallen-back empty owner \
         password would let the empty user password open it too"
    );
}

/// `encrypt_document` refuses to build a file neither password would
/// protect: reusing `Error::Other`, the same generic invalid-argument
/// variant `rotate_rewrite` uses for a bad `by`, rather than a new
/// variant.
#[test]
fn encrypt_document_refuses_when_both_passwords_are_empty() {
    let mut w = Writer::new(WriteOptions::default());
    let root = build(&mut w);
    let plain_bytes = w.finish(root).expect("plain document finishes");
    let plain = Document::load(plain_bytes).expect("plain document loads");

    let result = encrypt_document(&plain, "", "", Permissions::all(), WriteOptions::default());
    let Err(pdfboss_write::Error::Other(message)) = result else {
        panic!("expected Error::Other, got {result:?}");
    };
    assert!(
        message.contains("cannot both be empty"),
        "message: {message}"
    );
}

/// The encrypted bytes, opened with the user password, decrypt back to a
/// plain file: `Document::load` (no password at all) opens it, it carries
/// no `/Encrypt`, and the text still matches the original.
#[test]
fn decrypt_document_produces_a_plain_file_with_matching_text() {
    let (encryptor, dict) = encryptor();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let encrypted_bytes = w.finish(root).expect("encrypted document finishes");

    let opened =
        Document::load_with_password(encrypted_bytes, USER_PW).expect("user password opens");

    let plain_bytes =
        decrypt_document(&opened, WriteOptions::default()).expect("decrypt_document succeeds");

    let plain = Document::load(plain_bytes).expect("plain load succeeds with no password");
    assert!(
        !plain.is_encrypted(),
        "the decrypted output carries no /Encrypt"
    );
    assert_eq!(plain.page_count(), 1);
    let page = plain.page(0).expect("page 0 exists");
    let text = extract_text(&plain, &page, ReadingOrder::Content).expect("text extracts");
    assert!(text.contains("Hello, encrypted"), "{text:?}");
}

/// A document already opened under a password re-encrypts under new
/// passwords: its plaintext content copies across (the `is_locked` change
/// from refusing `is_encrypted`), the result opens under both new
/// passwords, and neither old password still works.
#[test]
fn encrypt_document_re_encrypts_a_password_opened_source_under_new_passwords() {
    let (encryptor, dict) = encryptor();
    let mut w = Writer::new_encrypted(WriteOptions::default(), encryptor, dict);
    let root = build(&mut w);
    let original_bytes = w.finish(root).expect("encrypted document finishes");

    let opened = Document::load_with_password(original_bytes, USER_PW)
        .expect("original user password opens");

    let new_user = "new-user-pw";
    let new_owner = "new-owner-pw";
    let re_encrypted = encrypt_document(
        &opened,
        new_user,
        new_owner,
        Permissions::all(),
        WriteOptions::default(),
    )
    .expect("re-encrypting a password-opened source succeeds");

    for old_password in [USER_PW, OWNER_PW] {
        assert!(
            matches!(
                Document::load_with_password(re_encrypted.clone(), old_password),
                Err(Error::Encrypted)
            ),
            "the old password {old_password:?} must no longer open the re-encrypted file"
        );
    }

    for new_password in [new_user, new_owner] {
        let reopened = Document::load_with_password(re_encrypted.clone(), new_password)
            .unwrap_or_else(|err| panic!("new password {new_password:?} opens: {err}"));
        assert_eq!(reopened.page_count(), 1);
        let page = reopened.page(0).expect("page 0 exists");
        let text = extract_text(&reopened, &page, ReadingOrder::Content).expect("text extracts");
        assert!(text.contains("Hello, encrypted"), "{text:?}");
    }
}

/// `/Length` is set from the object's serialized body, computed after
/// encryption: for a stream written raw (no compression) at a known
/// plaintext length `L`, the emitted `/Length` must equal the IV (16
/// bytes) plus the PKCS#7-padded ciphertext, never the plaintext length
/// itself. `L` here is already a multiple of 16, so padding must add a
/// full extra block rather than nothing.
#[test]
fn length_reflects_the_padded_ciphertext_not_the_plaintext() {
    let (encryptor, dict) = encryptor();
    let options = WriteOptions {
        xref: XrefStyle::Table,
        compress: false,
        object_streams: false,
        version: (1, 7),
    };
    let mut w = Writer::new_encrypted(options, encryptor, dict);
    let root = build(&mut w);
    let plaintext = vec![0x41u8; 32];
    assert_eq!(plaintext.len() % 16, 0, "the fixture must be block-aligned");
    let marker_ref = w.put_stream_raw(Dict::new(), plaintext.clone());
    let bytes = w.finish(root).expect("encrypted document finishes");

    let raw = parse_raw_object(&bytes, marker_ref);
    let raw_stream = raw.as_stream().expect("marker object is a stream");
    let length = raw_stream
        .dict
        .get_int("Length")
        .expect("stream carries a direct /Length");

    let l = plaintext.len();
    let padded = l + (16 - l % 16); // PKCS#7: a full extra block when l is already aligned
    let expected = 16 + padded; // IV prefix plus the padded ciphertext
    assert_eq!(
        length as usize, expected,
        "/Length must reflect the IV-prefixed, padded ciphertext, not the plaintext"
    );

    let doc = Document::load_with_password(bytes, USER_PW).expect("user password opens");
    let resolved = doc
        .resolve(&Object::Ref(marker_ref))
        .expect("marker object resolves");
    assert_eq!(
        resolved
            .as_stream()
            .expect("resolved marker is a stream")
            .data,
        plaintext,
        "the decrypted content must still equal the original plaintext"
    );
}

/// A source PDF whose `/Encrypt` dict carries `/EncryptMetadata false` and
/// whose metadata stream was stored in plaintext, the way a real writer
/// like Acrobat leaves it, assembled by hand since this crate's own
/// `Encryptor` never emits that entry. `rewrite_document` must carry the
/// metadata bytes through unchanged rather than corrupting them, the same
/// gap `pdfboss_core`'s decryptor now closes for a plain password load.
#[test]
fn rewrite_document_carries_an_encrypt_metadata_false_stream_through_unchanged() {
    const XMP: &[u8] =
        b"<?xpacket begin=''?><x:xmpmeta><dc:title>XMP Secret Title</dc:title></x:xmpmeta>\
          <?xpacket end='w'?>";

    let (mut enc, dict) =
        Encryptor::aes256_with_rng(USER_PW, OWNER_PW, Permissions::all(), counter_rng());

    let mut msg = Object::Dict({
        let mut d = Dict::new();
        d.insert(name("Msg"), Object::String(b"ordinary secret".to_vec()));
        d
    });
    enc.encrypt_object(&mut msg, 3, 0);
    let msg_bytes = msg
        .as_dict()
        .unwrap()
        .get("Msg")
        .and_then(Object::as_str_bytes)
        .unwrap();

    let mut b = PdfBuilder::new().version(1, 7);
    b.object(1, "<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
    b.object(3, &format!("<< /Msg {} >>", hexstr(msg_bytes)));
    b.stream(5, "<< /Type /Metadata /Subtype /XML >>", XMP);
    b.object(9, &encrypt_dict_body(&dict));
    let bytes = b.trailer_extra("/Encrypt 9 0 R").build(1);

    let doc = Document::load_with_password(bytes, USER_PW).expect("user password opens");
    let obj3 = doc.get(ObjRef { num: 3, gen: 0 }).unwrap();
    assert_eq!(
        obj3.as_dict()
            .unwrap()
            .get("Msg")
            .and_then(Object::as_str_bytes),
        Some(&b"ordinary secret"[..]),
        "an ordinary encrypted string still decrypts"
    );
    let obj5 = doc.get(ObjRef { num: 5, gen: 0 }).unwrap();
    assert_eq!(
        doc.stream_data(obj5.as_stream().unwrap()).unwrap(),
        XMP,
        "the metadata stream is plaintext before any rewrite"
    );

    let rewritten =
        rewrite_document(&doc, WriteOptions::default()).expect("rewrite_document succeeds");
    let plain = Document::load(rewritten).expect("rewritten output carries no /Encrypt");
    let root = plain
        .xref()
        .trailer
        .get_ref("Root")
        .expect("/Root present");
    let catalog = plain.get(root).expect("catalog resolves");
    let metadata_ref = catalog
        .as_dict()
        .unwrap()
        .get_ref("Metadata")
        .expect("/Metadata present on the rewritten catalog");
    let metadata_obj = plain.get(metadata_ref).expect("metadata object resolves");
    let metadata = plain
        .stream_data(metadata_obj.as_stream().expect("metadata is a stream"))
        .expect("metadata stream decodes");
    assert_eq!(
        metadata, XMP,
        "rewrite_document must carry the metadata bytes through unchanged"
    );
}
