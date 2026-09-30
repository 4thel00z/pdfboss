//! Async split parity: `copy_pages_with` over an `AsyncDocument` must emit
//! exactly the parts the synchronous `split_document` emits for the same
//! file, and its future must be `Send + 'static` so it can be spawned.
#![cfg(feature = "write")]

use pdfboss_aio::AsyncDocument;
use pdfboss_core::{Document, Page};
use pdfboss_testkit::multi_page_doc;
use pdfboss_write::{copy_pages_with, split_document, WriteOptions};

fn assert_send_static<T: Send + 'static>(_: &T) {}

#[tokio::test]
async fn async_parts_match_sync_split() {
    let bytes = multi_page_doc(&["one", "two", "three", "four", "five"]);
    let sync_parts = split_document(
        &Document::load(bytes.clone()).expect("sync doc loads"),
        2,
        WriteOptions::default(),
    )
    .expect("sync split succeeds");

    let doc = AsyncDocument::from_bytes(bytes)
        .await
        .expect("async doc opens");
    let pages: Vec<Page> = (0..doc.page_count())
        .map(|index| doc.page(index).expect("page exists"))
        .collect();
    let mut async_parts = Vec::new();
    for part in pages.chunks(2) {
        let future = copy_pages_with(
            doc.clone(),
            part.to_vec(),
            WriteOptions::default(),
            Vec::new(),
        );
        assert_send_static(&future);
        let spawned = tokio::spawn(future);
        async_parts.push(
            spawned
                .await
                .expect("the task completes")
                .expect("async copy succeeds"),
        );
    }

    assert_eq!(async_parts, sync_parts);
}
