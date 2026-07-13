//! Benchmarks for positional text extraction.
//!
//! Run with `cargo bench -p pdfboss-text`.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use pdfboss_core::Document;
use pdfboss_testkit::doc_with_graphics;
use pdfboss_text::extract_text;

/// A single page whose content stream shows `lines` lines of text.
fn text_doc(lines: usize) -> Vec<u8> {
    let mut content = String::from("BT /F1 12 Tf 72 720 Td ");
    for i in 0..lines {
        content.push_str(&format!("(The quick brown fox jumps {i}) Tj 0 -14 Td "));
    }
    content.push_str("ET");
    doc_with_graphics(&content)
}

fn bench_extract(c: &mut Criterion) {
    let bytes = text_doc(500);

    // Warm: document already loaded and its content stream cached; this
    // isolates the extraction algorithm itself.
    let doc = Document::load(bytes.clone()).unwrap();
    let page = doc.page(0).unwrap();
    c.bench_function("extract_text_warm_500_lines", |b| {
        b.iter(|| black_box(extract_text(&doc, &page).unwrap()));
    });

    // Cold: fresh document every iteration; captures load + decode + extract.
    c.bench_function("extract_text_cold_500_lines", |b| {
        b.iter_batched(
            || bytes.clone(),
            |data| {
                let doc = Document::load(data).unwrap();
                let page = doc.page(0).unwrap();
                black_box(extract_text(&doc, &page).unwrap())
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, bench_extract);
criterion_main!(benches);
