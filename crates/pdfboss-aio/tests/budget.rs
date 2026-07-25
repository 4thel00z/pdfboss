//! The huge-file guarantee: opening a multi-megabyte document and fetching
//! one object reads far less than the file — nothing ever reads it whole.

mod common;

use common::RecordingBackend;
use pdfboss_aio::{AsyncDocument, MemBackend};
use pdfboss_core::ObjRef;
use pdfboss_testkit::PdfBuilder;

fn multi_megabyte_doc() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
    );
    b.stream(4, "", b"BT (needle) Tj ET");
    // Multi-megabyte ballast between the useful objects and the xref.
    b.stream(9, "", &vec![b'x'; 3 * 1024 * 1024]);
    b.build(1)
}

#[tokio::test]
async fn opening_and_fetching_one_object_reads_less_than_64_kib() {
    let data = multi_megabyte_doc();
    assert!(data.len() > 3 * 1024 * 1024, "fixture is multi-megabyte");
    let (backend, log) = RecordingBackend::new(MemBackend::from(data));
    let doc = AsyncDocument::with_backend(backend).await.unwrap();
    let object = doc.get_object(ObjRef { num: 4, gen: 0 }).await.unwrap();
    assert!(object.as_stream().is_some());
    assert!(
        log.total_bytes() < 64 * 1024,
        "read {} bytes total; the budget is 64 KiB",
        log.total_bytes()
    );
    assert!(log.read_calls() > 0);
}
