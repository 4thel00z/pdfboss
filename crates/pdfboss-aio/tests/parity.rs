//! Byte-identical parity between the async and sync documents: objects,
//! streams, metadata, version, page count, and the full element sequence.

use flate2::write::ZlibEncoder;
use flate2::Compression;
use futures_util::StreamExt;
use pdfboss_aio::AsyncDocument;
use pdfboss_core::elements::ElementOpts;
use pdfboss_core::{Document, ObjRef};
use pdfboss_testkit::{hybrid_doc, multi_page_doc, objstm_payload, simple_doc, PdfBuilder};
use std::io::Write;

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let mut fixtures = vec![
        ("simple", simple_doc("parity")),
        ("multi_page", multi_page_doc(&["alpha", "beta", "gamma"])),
        ("hybrid", hybrid_doc()),
    ];
    let (dict, payload) = objstm_payload(&[
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ]);
    let mut b = PdfBuilder::new();
    b.stream(6, &dict, &payload);
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (compressed) Tj ET");
    fixtures.push(("objstm", b.build_xref_stream(1)));
    fixtures.push(("circular_font_ref", circular_font_ref_doc()));
    fixtures
}

/// A page whose `/Resources /Font /F1` entry is a self-referencing
/// object (`6 0 R`'s own content is literally `6 0 R`): resolving it loops
/// until the resolve-depth guard trips, yielding `Error::CircularReference`
/// — the one way `AsyncDocument::resolve`/`Document::resolve` can ever fail
/// on a resource-category entry (a missing or unreadable target instead
/// resolves leniently to `Null`). Core's `referenced_dict_entries` silently
/// skips such an entry (`let Ok(target) = ... else { continue }`); the
/// async logical layer must match exactly rather than surfacing a salvage
/// `Err`.
fn circular_font_ref_doc() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 6 0 R >> >> /Contents 4 0 R >>",
    );
    b.stream(4, "", b"BT ET");
    b.object(6, "6 0 R"); // self-reference: resolving /F1 hits CircularReference
    b.build(1)
}

/// Debug digest of one side's element sequence. `Err` items collapse to
/// "ERR": the two sides use different error types, and parity is about
/// what streams, not message text.
fn digest_sync(doc: &Document, opts: ElementOpts) -> Vec<String> {
    doc.elements(opts)
        .map(|item| match item {
            Ok(element) => format!("{element:?}"),
            Err(_) => "ERR".to_string(),
        })
        .collect()
}

async fn digest_async(doc: &AsyncDocument, opts: ElementOpts) -> Vec<String> {
    let mut stream = doc.elements(opts);
    let mut digest = Vec::new();
    while let Some(item) = stream.next().await {
        digest.push(match item {
            Ok(element) => format!("{element:?}"),
            Err(_) => "ERR".to_string(),
        });
    }
    digest
}

#[tokio::test]
async fn documents_agree_on_objects_streams_metadata_and_pages() {
    for (name, data) in fixtures() {
        let sync_doc = Document::load(data.clone()).unwrap();
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        assert_eq!(doc.version(), sync_doc.version(), "{name}: version");
        assert_eq!(doc.page_count(), sync_doc.page_count(), "{name}: pages");
        assert_eq!(
            doc.metadata().await.unwrap(),
            sync_doc.metadata(),
            "{name}: metadata"
        );
        for num in 1..=10u32 {
            let r = ObjRef { num, gen: 0 };
            match sync_doc.get(r) {
                Ok(expected) => {
                    let object = doc.get_object(r).await.unwrap();
                    assert_eq!(object, expected, "{name}: object {num}");
                    if let Some(stream) = object.as_stream() {
                        assert_eq!(
                            doc.decode_stream(stream).await.unwrap(),
                            sync_doc.stream_data(stream).unwrap(),
                            "{name}: stream {num}"
                        );
                    }
                }
                Err(_) => assert!(
                    doc.get_object(r).await.is_err(),
                    "{name}: object {num} must fail on both sides"
                ),
            }
        }
    }
}

#[tokio::test]
async fn full_element_sequences_are_identical() {
    let all = ElementOpts {
        physical: true,
        logical: true,
        pages: None,
        content_ops: true,
    };
    for (name, data) in fixtures() {
        let sync_doc = Document::load(data.clone()).unwrap();
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        for opts in [ElementOpts::default(), all.clone()] {
            let expected = digest_sync(&sync_doc, opts.clone());
            let streamed = digest_async(&doc, opts.clone()).await;
            assert_eq!(streamed, expected, "{name}: element sequence ({opts:?})");
        }
    }
}

#[tokio::test]
async fn circular_font_ref_is_skipped_not_erred_on_both_sides() {
    let data = circular_font_ref_doc();
    let sync_doc = Document::load(data.clone()).unwrap();
    let doc = AsyncDocument::from_bytes(data).await.unwrap();
    let opts = ElementOpts {
        physical: false,
        logical: true,
        pages: None,
        content_ops: false,
    };
    let sync_digest = digest_sync(&sync_doc, opts.clone());
    let async_digest = digest_async(&doc, opts).await;
    assert_eq!(
        async_digest, sync_digest,
        "circular font ref: full element-sequence parity"
    );
    assert!(
        !sync_digest.iter().any(|e| e == "ERR"),
        "core silently skips the circular ref (no Err element): {sync_digest:?}"
    );
    assert!(
        !async_digest.iter().any(|e| e == "ERR"),
        "async must also silently skip (no salvage Err element): {async_digest:?}"
    );
    // Only the Page element remains: the circular font entry contributes
    // nothing on either side.
    assert_eq!(sync_digest.len(), 1, "digest: {sync_digest:?}");
}

// --- Controller-required extra: indirect /DecodeParms bridging ---
//
// The filter pipeline resolves `/DecodeParms` synchronously (it takes a
// `&dyn Resolve`), so an *indirect* `/DecodeParms` reference exercises the
// async side's `prefetch_filter_refs`/`MapResolve` bridge: the document must
// fetch the referenced dict up front so `decode_stream` can hand the sync
// filter pipeline a resolver that already knows the answer. A trivial
// (parameterless) `/DecodeParms` dict wouldn't catch a broken bridge — an
// unresolved reference and a resolved-but-empty dict behave identically — so
// this fixture uses TIFF-predictor (`/Predictor 2`) parameters, whose actual
// numeric value changes the decoded bytes.

/// Forward TIFF-predictor (horizontal differencing, 8-bit, single row
/// spanning the whole payload): the inverse of what
/// `pdfboss_core::filters::predictor::apply` undoes.
fn tiff_predict(raw: &[u8]) -> Vec<u8> {
    let mut out = raw.to_vec();
    for i in (1..out.len()).rev() {
        out[i] = out[i].wrapping_sub(out[i - 1]);
    }
    out
}

/// Zlib-compresses `data`, mirroring the mechanism pdfboss-core's own
/// `filters::flate` tests use to build FlateDecode fixtures (that helper is
/// private to `pdfboss-core`, so this is a same-mechanism reimplementation
/// via the workspace's existing `flate2` dependency, not shared code).
fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

#[tokio::test]
async fn indirect_decode_parms_reference_decodes_identically() {
    let content: &[u8] = b"BT /F1 12 Tf 72 720 Td (indirect parms bridge) Tj ET";
    let predicted = tiff_predict(content);
    let compressed = zlib_compress(&predicted);

    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    // The DecodeParms dict lives in its own indirect object (6), so the
    // stream's /DecodeParms entry below is a reference, not an inline dict.
    b.object(6, &format!("<< /Predictor 2 /Columns {} >>", content.len()));
    b.stream(4, "/Filter /FlateDecode /DecodeParms 6 0 R", &compressed);
    let data = b.build(1);

    let sync_doc = Document::load(data.clone()).unwrap();
    let doc = AsyncDocument::from_bytes(data).await.unwrap();

    let r = ObjRef { num: 4, gen: 0 };
    let sync_object = sync_doc.get(r).unwrap();
    let sync_stream = sync_object.as_stream().unwrap();
    let sync_out = sync_doc.stream_data(sync_stream).unwrap();

    let async_object = doc.get_object(r).await.unwrap();
    let async_stream = async_object.as_stream().unwrap();
    let async_out = doc.decode_stream(async_stream).await.unwrap();

    assert_eq!(
        sync_out, content,
        "sync side must reconstruct the source text"
    );
    assert_eq!(
        async_out, sync_out,
        "indirect /DecodeParms must decode identically on both sides"
    );
}

/// A three-level tree whose `/MediaBox` (A4), `/Rotate` and `/CropBox` are
/// declared on `/Pages` nodes, never on the leaf: the attributes only reach
/// the page by inheritance (ISO 32000-1 7.7.3.4).
fn inherited_attrs_doc() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(
        2,
        "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 \
         /MediaBox [0 0 595 842] /Rotate 90 >>",
    );
    // An intermediate node overrides /Rotate and adds a /CropBox.
    b.object(
        3,
        "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 \
         /Rotate 180 /CropBox [10 10 300 400] >>",
    );
    b.object(4, "<< /Type /Page /Parent 3 0 R /Contents 5 0 R >>");
    b.stream(5, "", b"BT ET");
    b.object(6, "<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>");
    b.stream(7, "", b"BT ET");
    b.build(1)
}

/// Every page attribute the two APIs hand out must be identical: media box,
/// crop box, rotation, size, object reference and dictionary. The fixture
/// declares everything on `/Pages` nodes, so a traversal that fails to
/// inherit reports US Letter for an A4 page — silently, which is why this
/// compares every field rather than probing one.
#[tokio::test]
async fn pages_agree_with_the_sync_document() {
    let mut cases = fixtures();
    cases.push(("inherited_attrs", inherited_attrs_doc()));
    for (name, bytes) in cases {
        let sync_doc = Document::load(bytes.clone()).expect("sync load");
        let async_doc = AsyncDocument::from_bytes(bytes).await.expect("async open");
        assert_eq!(
            sync_doc.page_count(),
            async_doc.page_count(),
            "{name}: page counts"
        );
        for i in 0..sync_doc.page_count() {
            let s = sync_doc.page(i).expect("sync page");
            let a = async_doc.page(i).expect("async page");
            assert_eq!(s.media_box, a.media_box, "{name} page {i}: media box");
            assert_eq!(s.crop_box, a.crop_box, "{name} page {i}: crop box");
            assert_eq!(s.rotate, a.rotate, "{name} page {i}: rotation");
            assert_eq!(s.size(), a.size(), "{name} page {i}: size");
            assert_eq!(
                s.object_ref(),
                a.object_ref(),
                "{name} page {i}: object ref"
            );
            assert_eq!(s.dict(), a.dict(), "{name} page {i}: dict");
            assert_eq!(s.resources, a.resources, "{name} page {i}: resources");
        }
        let oob = async_doc.page(sync_doc.page_count());
        assert!(oob.is_err(), "{name}: out-of-bounds page errs");
    }
}

/// The whole point of the parity workstream, exercised end to end: the SAME
/// text-extraction and rendering implementations run over an
/// `AsyncDocument`, produce byte-identical output to the synchronous API,
/// and do so from inside `tokio::spawn` — which is the `Send + 'static`
/// gate enforced by a real runtime rather than a type assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_algorithms_run_over_the_async_document() {
    let bytes = multi_page_doc(&["alpha", "beta", "gamma"]);
    let sync_doc = Document::load(bytes.clone()).expect("sync load");
    let async_doc = AsyncDocument::from_bytes(bytes).await.expect("async open");

    for i in 0..sync_doc.page_count() {
        let sync_page = sync_doc.page(i).expect("sync page");
        let expected_text = pdfboss_output::extract_text(&sync_doc, &sync_page).expect("sync text");
        let (expected_pix, _) = pdfboss_render::render_page_reporting(
            &sync_doc,
            &sync_page,
            1.0,
            &pdfboss_render::RenderOptions::default(),
        )
        .expect("sync render");

        let doc = async_doc.clone();
        let handle = tokio::spawn(async move {
            let page = doc.page(i).expect("async page");
            let text = pdfboss_output::extract_text_with(doc.clone(), &page)
                .await
                .expect("async text");
            let opts = pdfboss_render::RenderOptions {
                oc: doc.oc_state().await.map(std::sync::Arc::new),
                ..Default::default()
            };
            let (pix, _) = pdfboss_render::render_page_reporting_with(doc, &page, 1.0, &opts)
                .await
                .expect("async render");
            (text, pix)
        });
        let (text, pix) = handle.await.expect("spawned task");
        assert_eq!(text, expected_text, "page {i}: extracted text");
        assert_eq!(
            pix.data, expected_pix.data,
            "page {i}: rendered pixels must be byte-identical"
        );
    }
}

/// A document with optional content renders identically over both
/// documents once the async caller passes `AsyncDocument::oc_state` through
/// the render options — the synchronous entry reads the same configuration
/// itself. Leaving the options bare paints the hidden layer, so the
/// comparison genuinely exercises the gate.
#[tokio::test]
async fn optional_content_renders_identically() {
    let mut b = PdfBuilder::new();
    b.object(
        1,
        "<< /Type /Catalog /Pages 2 0 R /OCProperties \
         << /OCGs [8 0 R] /D << /OFF [8 0 R] >> >> >>",
    );
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
         /Resources << /Properties << /H 8 0 R >> >> /Contents 4 0 R >>",
    );
    b.stream(4, "", b"1 0 0 rg /OC /H BDC 10 10 80 80 re f EMC");
    b.object(8, "<< /Type /OCG /Name (layer) >>");
    let bytes = b.build(1);

    let sync_doc = Document::load(bytes.clone()).expect("sync load");
    let sync_page = sync_doc.page(0).expect("sync page");
    let (expected, report) = pdfboss_render::render_page_reporting(
        &sync_doc,
        &sync_page,
        1.0,
        &pdfboss_render::RenderOptions::default(),
    )
    .expect("sync render");
    assert_eq!(report.hidden, 1, "the sync entry reads the configuration");

    let async_doc = AsyncDocument::from_bytes(bytes).await.expect("async open");
    let page = async_doc.page(0).expect("async page");
    let opts = pdfboss_render::RenderOptions {
        oc: async_doc.oc_state().await.map(std::sync::Arc::new),
        ..Default::default()
    };
    let (pix, report) =
        pdfboss_render::render_page_reporting_with(async_doc.clone(), &page, 1.0, &opts)
            .await
            .expect("async render");
    assert_eq!(report.hidden, 1);
    assert_eq!(pix.data, expected.data, "pixels must be byte-identical");

    let (bare, _) = pdfboss_render::render_page_reporting_with(
        async_doc,
        &page,
        1.0,
        &pdfboss_render::RenderOptions::default(),
    )
    .await
    .expect("bare async render");
    assert_ne!(
        bare.data, expected.data,
        "without the state the hidden layer paints, so the parity above is real"
    );
}

/// An RC4-encrypted document (Standard handler, empty user password) opens
/// asynchronously and decrypts identically to the synchronous document:
/// strings, stream data, and extracted text.
#[tokio::test]
async fn encrypted_documents_decrypt_identically() {
    let bytes = pdfboss_testkit::encrypted_rc4_doc("Top secret message");
    let sync_doc = Document::load(bytes.clone()).expect("sync opens the file");
    let async_doc = AsyncDocument::from_bytes(bytes)
        .await
        .expect("async opens the file");

    // The encrypted /Msg string decrypts to the plaintext on both sides.
    let msg_ref = ObjRef { num: 6, gen: 0 };
    let sync_msg = sync_doc.get(msg_ref).expect("sync object");
    let async_msg = async_doc.get_object(msg_ref).await.expect("async object");
    assert_eq!(sync_msg, async_msg, "decrypted dictionaries agree");
    assert_eq!(
        async_msg
            .as_dict()
            .unwrap()
            .get("Msg")
            .unwrap()
            .as_str_bytes(),
        Some(b"Top secret message".as_slice()),
        "the string decrypts to its plaintext"
    );

    // The encrypted content stream decrypts, so text extraction agrees.
    let sync_page = sync_doc.page(0).expect("sync page");
    let async_page = async_doc.page(0).expect("async page");
    let sync_text = pdfboss_output::extract_text(&sync_doc, &sync_page).expect("sync text");
    let async_text = pdfboss_output::extract_text_with(async_doc.clone(), &async_page)
        .await
        .expect("async text");
    assert_eq!(sync_text, "Top secret message");
    assert_eq!(
        sync_text, async_text,
        "extraction agrees on encrypted files"
    );

    // Markdown extraction over the same encrypted content agrees too.
    let sync_md = pdfboss_output::extract_page_markdown(&sync_doc, &sync_page).expect("sync md");
    let async_md = pdfboss_output::extract_page_markdown_with(async_doc.clone(), &async_page)
        .await
        .expect("async md");
    assert_eq!(sync_md, async_md, "markdown agrees on encrypted files");
}
