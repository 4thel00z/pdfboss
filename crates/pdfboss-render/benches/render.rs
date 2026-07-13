//! Benchmarks for page rasterization.
//!
//! Run with `cargo bench -p pdfboss-render`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pdfboss_core::Document;
use pdfboss_render::render_page;
use pdfboss_testkit::doc_with_graphics;

/// A page covered in `n` small filled rectangles in varying colors.
fn rects_doc(n: usize) -> Vec<u8> {
    let mut content = String::new();
    for i in 0..n {
        let x = (i % 50) as f32 * 12.0;
        let y = (i / 50) as f32 * 12.0;
        let r = (i % 7) as f32 / 7.0;
        let g = (i % 5) as f32 / 5.0;
        let b = (i % 3) as f32 / 3.0;
        content.push_str(&format!("{r} {g} {b} rg {x} {y} 10 10 re f "));
    }
    doc_with_graphics(&content)
}

/// A page with filled Bezier curves and strokes, stressing the coverage and
/// stroke paths rather than axis-aligned rectangles.
fn curves_doc(n: usize) -> Vec<u8> {
    let mut content = String::from("2 w ");
    for i in 0..n {
        let x = (i % 40) as f32 * 15.0 + 20.0;
        let y = (i / 40) as f32 * 15.0 + 20.0;
        content.push_str(&format!(
            "0.2 0.4 0.8 rg {x} {y} m {} {} {} {} {} {} c f \
             0 0 0 RG {x} {y} m {} {} l S ",
            x + 10.0,
            y + 30.0,
            x + 40.0,
            y + 30.0,
            x + 50.0,
            y,
            x + 60.0,
            y + 60.0,
        ));
    }
    doc_with_graphics(&content)
}

/// A page that sets one clip, then saves/restores graphics state `n` times,
/// filling under the inherited clip each time. Stresses the per-`q` clone of
/// the graphics state (and thus the clip mask).
fn nested_clip_doc(n: usize) -> Vec<u8> {
    let mut content = String::from("0 0 400 400 re W n ");
    for i in 0..n {
        let x = (i % 30) as f32 * 12.0;
        content.push_str(&format!("q 0.4 0.5 0.6 rg {x} 20 50 50 re f Q "));
    }
    doc_with_graphics(&content)
}

fn bench_render(c: &mut Criterion) {
    let rects = rects_doc(1000);
    let doc = Document::load(rects).unwrap();
    let page = doc.page(0).unwrap();
    c.bench_function("render_1000_rects_scale2", |b| {
        b.iter(|| black_box(render_page(&doc, &page, 2.0).unwrap()));
    });

    let curves = curves_doc(400);
    let doc2 = Document::load(curves).unwrap();
    let page2 = doc2.page(0).unwrap();
    c.bench_function("render_400_curves_scale2", |b| {
        b.iter(|| black_box(render_page(&doc2, &page2, 2.0).unwrap()));
    });

    let clips = nested_clip_doc(2000);
    let doc3 = Document::load(clips).unwrap();
    let page3 = doc3.page(0).unwrap();
    c.bench_function("render_2000_nested_clips_scale2", |b| {
        b.iter(|| black_box(render_page(&doc3, &page3, 2.0).unwrap()));
    });
}

criterion_group!(benches, bench_render);
criterion_main!(benches);
