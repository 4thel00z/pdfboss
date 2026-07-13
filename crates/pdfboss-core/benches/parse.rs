//! Benchmarks for document loading, page-tree walking and stream inflation.
//!
//! Inputs are built with the fixture builder so the benchmarks stay in-repo
//! and reproducible. Run with `cargo bench -p pdfboss-core`.

use std::io::Write as _;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use flate2::{write::ZlibEncoder, Compression};
use pdfboss_core::{Document, ObjRef};
use pdfboss_testkit::{multi_page_doc, PdfBuilder};

/// A document with `n` pages, each showing a short line of text.
fn multipage_bytes(n: usize) -> Vec<u8> {
    let texts: Vec<String> = (0..n)
        .map(|i| format!("Page {i}: the quick brown fox jumps over the lazy dog {i}"))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    multi_page_doc(&refs)
}

/// A document holding one `FlateDecode` stream whose decoded payload is
/// `payload_len` bytes of semi-repetitive data.
fn flate_doc(payload_len: usize) -> Vec<u8> {
    let payload: Vec<u8> = (0..payload_len)
        .map(|i| (i.wrapping_mul(31).wrapping_add(i / 7)) as u8)
        .collect();
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&payload).unwrap();
    let compressed = enc.finish().unwrap();

    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
    b.stream(3, "/Filter /FlateDecode", &compressed);
    b.build(1)
}

fn bench_parse(c: &mut Criterion) {
    let mp = multipage_bytes(300);
    let mut g = c.benchmark_group("parse");
    g.throughput(Throughput::Bytes(mp.len() as u64));
    g.bench_function("load_300_pages", |b| {
        b.iter_batched(
            || mp.clone(),
            |data| Document::load(black_box(data)).unwrap(),
            BatchSize::SmallInput,
        );
    });
    g.bench_function("load_and_walk_300_pages", |b| {
        b.iter_batched(
            || mp.clone(),
            |data| {
                let doc = Document::load(data).unwrap();
                for i in 0..doc.page_count() {
                    black_box(doc.page(i).unwrap());
                }
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

fn bench_filter(c: &mut Criterion) {
    let len = 1usize << 20; // 1 MiB decoded
    let doc = Document::load(flate_doc(len)).unwrap();
    let obj = doc.get(ObjRef { num: 3, gen: 0 }).unwrap();

    let mut g = c.benchmark_group("filter");
    g.throughput(Throughput::Bytes(len as u64));
    g.bench_function("flate_decode_1mib", |b| {
        b.iter(|| {
            let s = obj.as_stream().unwrap();
            black_box(doc.stream_data(s).unwrap())
        });
    });
    g.finish();
}

criterion_group!(benches, bench_parse, bench_filter);
criterion_main!(benches);
