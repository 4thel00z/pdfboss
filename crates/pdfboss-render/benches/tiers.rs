//! Benchmarks comparing the three [`GlyphPainting`] tiers across the font
//! shapes whose painting actually differs by tier.
//!
//! Run with `cargo bench -p pdfboss-render --bench tiers`.
//!
//! Each group renders the *same* page at `EmbeddedTrueTypeOnly`,
//! `AllEmbedded`, and `Full`, so the timings read directly as "what does this
//! tier cost on this workload":
//!
//! - **`truetype_text`** — a page whose glyphs are an embedded `FontFile2`
//!   (a real bundled OFL face). TrueType paints at every tier, so the three
//!   timings should be ~flat: the render gate itself adds no work.
//! - **`type3_text`** — a page whose glyphs are Type3 `/CharProcs`. These are
//!   blank at `EmbeddedTrueTypeOnly` (cheap: nothing painted) and painted at
//!   `AllEmbedded`/`Full`. The delta is the Type3 content-stream recursion
//!   cost.
//! - **`substitute_text`** — a page with a non-embedded `/Helvetica`. Blank
//!   until `Full` with a substitute source (here a `Dir` provider over the
//!   bundled faces). The `Full` timing over `AllEmbedded` is the substitution
//!   (parse + map + paint the substitute face) cost.

use std::path::PathBuf;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pdfboss_core::{Document, Page};
use pdfboss_render::{render_page_with_options, GlyphPainting, RenderOptions, SubstituteSource};
use pdfboss_testkit::PdfBuilder;

/// A real bundled OFL TrueType face, embedded into the fixture as a
/// `FontFile2` so the TrueType glyph pipeline runs on realistic outlines.
const TINOS: &[u8] = include_bytes!("../assets/fonts/Tinos-Regular.ttf");

/// Directory the `Dir` substitute provider reads faces from (the bundled
/// OFL set). Resolved from the manifest dir so it is independent of the
/// benchmark's working directory.
fn assets_font_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts"))
}

/// Lines of text per fixture page (each ~54 glyphs), sized so a render is a
/// few hundred glyphs — enough to dominate fixed per-page overhead.
const LINES: usize = 40;
const SENTENCE: &str = "The quick brown fox jumps over the lazy dog 0123456789";

/// Catalog(1) -> Pages(2) -> Page(3) referencing font 5 and content 4, on a
/// US-letter media box. The caller adds object 5 (the font) and any
/// descendants.
fn page_scaffold(b: &mut PdfBuilder, content: &[u8]) {
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
    );
    b.stream(4, "", content);
}

/// `LINES` lines of `text`, each placed with an absolute text matrix at 12pt.
fn text_content(text: &str) -> Vec<u8> {
    let mut c = String::from("BT /F0 12 Tf ");
    for i in 0..LINES {
        let y = 780 - (i as i32) * 18;
        c.push_str(&format!("1 0 0 1 36 {y} Tm ({text}) Tj "));
    }
    c.push_str("ET");
    c.into_bytes()
}

/// A page showing `SENTENCE` in an embedded `FontFile2` (bundled Tinos).
fn truetype_doc() -> Vec<u8> {
    let mut b = PdfBuilder::new().version(1, 5);
    page_scaffold(&mut b, &text_content(SENTENCE));
    b.object(
        5,
        "<< /Type /Font /Subtype /TrueType /BaseFont /Tinos-Regular \
         /FontDescriptor 6 0 R /Encoding /WinAnsiEncoding >>",
    );
    b.object(
        6,
        "<< /Type /FontDescriptor /FontName /Tinos-Regular /Flags 34 \
         /FontFile2 7 0 R >>",
    );
    b.stream(7, &format!("/Length1 {}", TINOS.len()), TINOS);
    b.build(1)
}

/// A page showing the same amount of text in a non-embedded `/Helvetica`
/// (no `FontFile*`): blank until `Full` substitutes a bundled face.
fn non_embedded_doc() -> Vec<u8> {
    let mut b = PdfBuilder::new().version(1, 5);
    page_scaffold(&mut b, &text_content(SENTENCE));
    b.object(
        5,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
         /FontDescriptor 6 0 R /Encoding /WinAnsiEncoding >>",
    );
    b.object(
        6,
        "<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 >>",
    );
    b.build(1)
}

/// A page whose glyphs are Type3 `/CharProcs`: codes `A`..`E` map to five
/// content-stream glyphs, each a filled box. Each shown code re-enters the
/// executor.
fn type3_doc() -> Vec<u8> {
    let mut b = PdfBuilder::new().version(1, 5);
    let glyphs = "ABCDE".repeat(10); // 50 Type3 glyphs per line
    page_scaffold(&mut b, &text_content(&glyphs));
    b.object(
        5,
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
         /FontMatrix [0.001 0 0 0.001 0 0] \
         /CharProcs << /g0 10 0 R /g1 11 0 R /g2 12 0 R /g3 13 0 R /g4 14 0 R >> \
         /Encoding << /Differences [65 /g0 /g1 /g2 /g3 /g4] >> \
         /FirstChar 65 /LastChar 69 /Widths [600 600 600 600 600] \
         /Resources << >> >>",
    );
    for (j, num) in (10..15).enumerate() {
        let x = j * 60;
        // `d0` colored glyph: set the width, then fill a box in glyph space.
        let proc = format!("1000 0 d0 {x} 0 400 700 re f");
        b.stream(num as u32, "", proc.as_bytes());
    }
    b.build(1)
}

/// Renders `page` at every tier of `tiers` under one benchmark group.
fn bench_group(
    c: &mut Criterion,
    name: &str,
    doc: &Document,
    page: &Page,
    tiers: &[(&str, GlyphPainting, SubstituteSource)],
) {
    let mut group = c.benchmark_group(name);
    for (label, painting, substitutes) in tiers {
        let opts = RenderOptions {
            glyph_painting: *painting,
            substitutes: substitutes.clone(),
        };
        group.bench_function(*label, |b| {
            b.iter(|| black_box(render_page_with_options(doc, page, 1.0, &opts).unwrap()));
        });
    }
    group.finish();
}

fn bench_tiers(c: &mut Criterion) {
    let all_three = |subst_full: SubstituteSource| {
        [
            (
                "embedded-only",
                GlyphPainting::EmbeddedTrueTypeOnly,
                SubstituteSource::None,
            ),
            (
                "all-embedded",
                GlyphPainting::AllEmbedded,
                SubstituteSource::None,
            ),
            ("full", GlyphPainting::Full, subst_full),
        ]
    };

    // TrueType: paints at every tier -> the gate should be free (flat).
    let tt = Document::load(truetype_doc()).expect("load truetype doc");
    let tt_page = tt.page(0).expect("page");
    bench_group(
        c,
        "truetype_text",
        &tt,
        &tt_page,
        &all_three(SubstituteSource::None),
    );

    // Type3: blank at EmbeddedTrueTypeOnly, painted at AllEmbedded/Full.
    let t3 = Document::load(type3_doc()).expect("load type3 doc");
    let t3_page = t3.page(0).expect("page");
    bench_group(
        c,
        "type3_text",
        &t3,
        &t3_page,
        &all_three(SubstituteSource::None),
    );

    // Non-embedded: blank until Full substitutes bundled faces from a Dir.
    let ne = Document::load(non_embedded_doc()).expect("load non-embedded doc");
    let ne_page = ne.page(0).expect("page");
    bench_group(
        c,
        "substitute_text",
        &ne,
        &ne_page,
        &all_three(SubstituteSource::Dir(assets_font_dir())),
    );
}

criterion_group!(benches, bench_tiers);
criterion_main!(benches);
