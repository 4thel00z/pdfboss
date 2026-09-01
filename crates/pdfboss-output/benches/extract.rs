//! Benchmarks for positional text extraction and layout.
//!
//! Run with `cargo bench -p pdfboss-output`.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use pdfboss_core::Document;
use pdfboss_output::{extract_text, ReadingOrder};
use pdfboss_testkit::doc_with_graphics;

/// A single page whose content stream shows `lines` lines of text, wrapped
/// into columns of 45 so every line stays inside the page box — extraction
/// clips to it, and a fixture running off the page measures less than it
/// claims.
fn text_doc(lines: usize) -> Vec<u8> {
    let mut content = String::from("BT /F1 12 Tf ");
    for i in 0..lines {
        let x = 72 + (i / 45) * 42;
        let y = 720 - (i % 45) * 14;
        content.push_str(&format!(
            "1 0 0 1 {x} {y} Tm (The quick brown fox jumps {i}) Tj "
        ));
    }
    content.push_str("ET");
    doc_with_graphics(&content)
}

/// A single page of `lines` justified lines, each written as `words`
/// separately positioned spans, the shape a typesetter's content stream
/// takes and the one that makes line grouping's cost visible.
fn dense_doc(lines: usize, words: usize) -> Vec<u8> {
    let mut content = String::from("BT /F1 10 Tf ");
    for line in 0..lines {
        let y = 760.0 - line as f32 * 12.0;
        for word in 0..words {
            let x = 72.0 + word as f32 * 11.5;
            content.push_str(&format!("1 0 0 1 {x} {y} Tm (w{word}) Tj "));
        }
    }
    content.push_str("ET");
    doc_with_graphics(&content)
}

/// A single page of `flows` separate text blocks, each of `lines` lines,
/// written bottom block first so every block opens a new flow — the shape
/// that exercises the flow-order pass rather than a single monolithic flow.
fn flowy_doc(flows: usize, lines: usize) -> Vec<u8> {
    let mut content = String::from("BT /F1 9 Tf ");
    for flow in (0..flows).rev() {
        let top = 770.0 - flow as f32 * 19.0;
        for line in 0..lines {
            let y = top - line as f32 * 9.0;
            content.push_str(&format!(
                "1 0 0 1 72 {y} Tm (Block {flow} body line {line} of text) Tj "
            ));
        }
    }
    content.push_str("ET");
    doc_with_graphics(&content)
}

fn bench_extract(c: &mut Criterion) {
    let dense = Document::load(dense_doc(60, 40)).unwrap();
    let dense_page = dense.page(0).unwrap();
    c.bench_function("extract_text_warm_dense_60x40", |b| {
        b.iter(|| black_box(extract_text(&dense, &dense_page, ReadingOrder::Content).unwrap()));
    });

    let flowy = Document::load(flowy_doc(40, 2)).unwrap();
    let flowy_page = flowy.page(0).unwrap();
    c.bench_function("extract_text_warm_flows_40x2", |b| {
        b.iter(|| black_box(extract_text(&flowy, &flowy_page).unwrap()));
    });

    let bytes = text_doc(500);

    // Warm: document already loaded and its content stream cached; this
    // isolates the extraction algorithm itself.
    let doc = Document::load(bytes.clone()).unwrap();
    let page = doc.page(0).unwrap();
    c.bench_function("extract_text_warm_500_lines", |b| {
        b.iter(|| black_box(extract_text(&doc, &page, ReadingOrder::Content).unwrap()));
    });

    // Cold: fresh document every iteration; captures load + decode + extract.
    c.bench_function("extract_text_cold_500_lines", |b| {
        b.iter_batched(
            || bytes.clone(),
            |data| {
                let doc = Document::load(data).unwrap();
                let page = doc.page(0).unwrap();
                black_box(extract_text(&doc, &page, ReadingOrder::Content).unwrap())
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, bench_extract);
criterion_main!(benches);
