//! Async incremental-append parity: `overlay_base` plus `append_overlay`
//! over an `AsyncDocument` must produce exactly the same bytes as the
//! synchronous `Update` over the same base and the same edits, and must
//! refuse an encrypted base the same way the synchronous path does.
#![cfg(feature = "write")]

use pdfboss_aio::{append_overlay, overlay_base, AsyncDocument};
use pdfboss_core::{Dict, Document, Name, ObjRef, Object};
use pdfboss_write::{
    set_metadata_with, Error, Immediate, Metadata, Overlay, Page, PageSize, Pdf, Standard14,
    Update, WriteOptions, XrefStyle,
};

/// A classic-table base document with an existing `/Info /Title` and a
/// catalog `/Metadata` packet (every `Pdf` with metadata writes one), so
/// the parity test exercises both the info-dict merge and the XMP rewrite.
fn classic_base_with_title(title: &str) -> Vec<u8> {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("Base page", 72.0, 700.0, Standard14::Helvetica, 14.0)
        .unwrap();
    Pdf {
        pages: vec![page],
        metadata: Some(Metadata {
            title: Some(title.to_string()),
            ..Metadata::default()
        }),
        options: WriteOptions {
            xref: XrefStyle::Table,
            ..WriteOptions::default()
        },
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap()
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "pdfboss-aio-update-test-{name}-{}.pdf",
        std::process::id()
    ))
}

/// `dict` with every value resolved against `doc`, mirroring
/// `pdfboss_write::Update`'s own `resolve_dict`: an indirect value reads
/// back as the object it points to, so a field the merge keeps still
/// carries text rather than a bare reference.
async fn resolve_dict(doc: &AsyncDocument, dict: &Dict) -> Dict {
    let mut out = Dict::new();
    for (key, value) in dict.iter() {
        let resolved = doc.resolve(value).await.unwrap_or_else(|_| value.clone());
        out.insert(key.clone(), resolved);
    }
    out
}

/// `existing_info` for [`set_metadata_with`], mirroring
/// `pdfboss_write::Update::set_metadata`: the base's `/Info` dictionary,
/// fetched and resolved through `doc`, alongside its own reference.
async fn existing_info(doc: &AsyncDocument, info: Option<ObjRef>) -> Option<(ObjRef, Dict)> {
    let r = info?;
    let object = doc.get_object(r).await.ok()?;
    let dict = object.as_dict()?.clone();
    Some((r, resolve_dict(doc, &dict).await))
}

/// The catalog's `/Metadata` entry, when it is an indirect reference,
/// mirroring `pdfboss_write::Update::catalog_metadata_ref`.
async fn catalog_metadata_ref(doc: &AsyncDocument, root: ObjRef) -> Option<ObjRef> {
    let catalog = doc.get_object(root).await.ok()?;
    match catalog.as_dict()?.get("Metadata")? {
        Object::Ref(r) => Some(*r),
        _ => None,
    }
}

#[tokio::test]
async fn async_append_matches_sync_bytes() {
    let base = classic_base_with_title("Old");
    let path = temp_path("parity");
    std::fs::write(&path, &base).unwrap();

    let sync_doc = Document::load(base).unwrap();
    let mut sync_update = Update::new(&sync_doc).unwrap();
    sync_update
        .set_metadata(Metadata {
            title: Some("Renamed".to_string()),
            ..Metadata::default()
        })
        .unwrap();
    let sync_bytes = sync_update.appended().unwrap();

    let async_doc = AsyncDocument::open(&path).await.unwrap();
    std::fs::remove_file(&path).ok();
    let base_info = overlay_base(&async_doc).await.unwrap();
    let info_ref = base_info.info;
    let root = base_info.root;

    let mut overlay = Overlay::new(base_info);
    let existing_info = existing_info(&async_doc, info_ref).await;
    let xmp_ref = catalog_metadata_ref(&async_doc, root).await;
    set_metadata_with(
        &mut overlay,
        existing_info,
        xmp_ref,
        Metadata {
            title: Some("Renamed".to_string()),
            ..Metadata::default()
        },
    )
    .unwrap();

    let async_bytes = append_overlay(&async_doc, &overlay, Vec::new())
        .await
        .unwrap();

    assert_eq!(sync_bytes, async_bytes);
}

/// `start_offset`'s pad rule: an object header may not directly follow a
/// base that does not already end on a line terminator, so a `\n` goes in
/// first. `Writer::finish` never ends a file on one (bases end on
/// `%%EOF`, per `writer::tests::table_mode_minimal_document_loads`), and
/// popping one more byte off only trims that non-terminator further, so
/// the base still needs the pad and still loads (`startxref` is found by
/// its own keyword, not by scanning for `%%EOF`). The pad must land at the
/// same offset, `base.len()`, whether the base is opened synchronously or
/// asynchronously.
#[tokio::test]
async fn pad_byte_matches_sync_and_async_over_a_truncated_base() {
    let mut base = classic_base_with_title("Old");
    base.pop();
    let path = temp_path("pad-rule");
    std::fs::write(&path, &base).unwrap();

    let sync_doc = Document::load(base.clone()).unwrap();
    let mut sync_update = Update::new(&sync_doc).unwrap();
    sync_update
        .set_metadata(Metadata {
            title: Some("Renamed".to_string()),
            ..Metadata::default()
        })
        .unwrap();
    let sync_bytes = sync_update.appended().unwrap();

    let async_doc = AsyncDocument::open(&path).await.unwrap();
    std::fs::remove_file(&path).ok();
    let base_info = overlay_base(&async_doc).await.unwrap();
    let info_ref = base_info.info;
    let root = base_info.root;

    let mut overlay = Overlay::new(base_info);
    let existing_info = existing_info(&async_doc, info_ref).await;
    let xmp_ref = catalog_metadata_ref(&async_doc, root).await;
    set_metadata_with(
        &mut overlay,
        existing_info,
        xmp_ref,
        Metadata {
            title: Some("Renamed".to_string()),
            ..Metadata::default()
        },
    )
    .unwrap();

    let async_bytes = append_overlay(&async_doc, &overlay, Vec::new())
        .await
        .unwrap();

    assert_eq!(
        sync_bytes, async_bytes,
        "sync and async agree on the padded, appended bytes over a truncated base"
    );
    assert_eq!(
        sync_bytes[base.len()],
        b'\n',
        "the pad byte lands right after the truncated base, before the appended section"
    );
}

/// A writer that shares its buffer through `Rc`, wrapped in `Immediate` so
/// it satisfies `AsyncByteSink`: `append_overlay` only returns its sink
/// argument on success, so a test that must inspect what reached the sink
/// after an error needs a handle that survives the dropped argument.
#[derive(Debug)]
struct SharedWriter(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A name containing NUL has no legal escape (`pdfboss_write::ser` rejects
/// `#00`), so building the appended section must fail; that failure must
/// happen before any byte reaches the sink, not partway through streaming
/// the base.
#[tokio::test]
async fn section_build_failure_leaves_sink_empty() {
    let base = classic_base_with_title("Old");
    let path = temp_path("nul-name");
    std::fs::write(&path, &base).unwrap();
    let doc = AsyncDocument::open(&path).await.unwrap();
    std::fs::remove_file(&path).ok();

    let base_info = overlay_base(&doc).await.unwrap();
    let mut overlay = Overlay::new(base_info);
    overlay.put(Object::Name(Name("bad\0name".to_string())));

    let shared = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = Immediate(SharedWriter(std::rc::Rc::clone(&shared)));

    let result = append_overlay(&doc, &overlay, sink).await;

    assert!(
        matches!(result, Err(Error::Other(_))),
        "a name containing NUL must fail to serialize: {result:?}"
    );
    assert!(
        shared.borrow().is_empty(),
        "the sink must receive nothing when the section fails to build"
    );
}

#[tokio::test]
async fn encrypted_async_base_is_refused() {
    let bytes = pdfboss_testkit::encrypted_rc4_doc("secret");
    let path = temp_path("encrypted");
    std::fs::write(&path, &bytes).unwrap();
    let doc = AsyncDocument::open(&path).await.unwrap();
    std::fs::remove_file(&path).ok();
    assert!(matches!(
        overlay_base(&doc).await,
        Err(Error::EncryptedBase)
    ));
}
