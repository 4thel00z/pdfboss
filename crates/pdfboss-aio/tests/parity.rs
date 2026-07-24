//! Byte-identical parity between the async and sync documents: objects,
//! streams, metadata, version, page count, and the full element sequence.

use flate2::write::ZlibEncoder;
use flate2::Compression;
use futures_util::StreamExt;
use pdfboss_aio::AsyncDocument;
use pdfboss_core::elements::ElementOpts;
use pdfboss_core::{Document, ObjRef};
use pdfboss_testkit::{multi_page_doc, objstm_payload, simple_doc, PdfBuilder};
use std::io::Write;

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let mut fixtures = vec![
        ("simple", simple_doc("parity")),
        ("multi_page", multi_page_doc(&["alpha", "beta", "gamma"])),
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
    fixtures
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
