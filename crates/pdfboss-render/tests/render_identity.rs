//! Byte-identity harness: every fixture below renders to a recorded digest.
//!
//! # What this is for
//!
//! The renderer is about to be rewritten — its recursion into form XObjects and
//! Type3 CharProcs becomes an explicit work stack so that one implementation can
//! serve both the synchronous and the asynchronous API. That rewrite touches
//! operator dispatch, graphics-state save/restore, the resource chain, and the
//! order content is painted in. Every one of those can change *pixels* while
//! leaving the existing tests green, because those tests probe individual pixels
//! they chose in advance — they answer "is this box dark", not "did this page
//! change".
//!
//! So this asserts the whole framebuffer. A digest mismatch means the render
//! changed; the assertion prints enough to say how.
//!
//! # Reading a failure
//!
//! The message carries dimensions, ink coverage, and the digest. Dimensions
//! moving means geometry; coverage moving means something started or stopped
//! painting; coverage identical with a different digest means the same amount of
//! ink landed somewhere else, or in a different colour.
//!
//! # When a digest legitimately changes
//!
//! Only when the render is *meant* to change. Recording a new value is a
//! deliberate act that belongs in the commit that changes the output, with the
//! reason in its message. Updating one to make a red test green defeats the entire
//! purpose of the file.
//!
//! # Coverage
//!
//! Each case exists because it exercises something the rewrite can break. Between
//! them they cover nested forms with `/Matrix` and `/BBox`, a form's own
//! `/Resources` shadowing the page's, Type3 `d0`/`d1` colour behaviour and glyph
//! ordering, clipping (including the intersection of nested clips), the `q`/`Q`
//! stack across a form boundary, an image XObject with an `/Indexed` palette, an
//! `/ImageMask` stencil, and the four device colour spaces.

use pdfboss_core::Document;
use pdfboss_render::{render_page_with_options, GlyphPainting, Pixmap, RenderOptions};
use pdfboss_testkit::PdfBuilder;

/// FNV-1a over the pixel buffer.
///
/// Written out here rather than reusing `pdfboss_core::hash::FxHasher` on purpose.
/// That hasher's output is an implementation detail free to change for its own
/// reasons, and if it did, every digest in this file would break while the
/// renderer was blameless. A hash defined in the file that stores its outputs
/// cannot drift out from under them.
fn digest(pix: &Pixmap) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for chunk in [
        &pix.width.to_le_bytes()[..],
        &pix.height.to_le_bytes()[..],
        &pix.data[..],
    ] {
        for &b in chunk {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Fraction of pixels that are not the white background, as a percentage with one
/// decimal. Reported on failure because it separates "geometry moved" from
/// "something stopped painting".
fn ink(pix: &Pixmap) -> String {
    let inked = pix
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] != 255 || p[1] != 255 || p[2] != 255)
        .count();
    let total = (pix.width * pix.height) as usize;
    format!("{:.1}%", 100.0 * inked as f64 / total.max(1) as f64)
}

/// A page of content over a 200x200 media box, with `resources` as the page's
/// `/Resources` and `extra` as raw additional objects appended verbatim.
fn page_doc(
    resources: &str,
    content: &[u8],
    extra: &[(u32, &str)],
    streams: &[(u32, &str, &[u8])],
) -> Vec<u8> {
    let mut b = PdfBuilder::new().version(1, 5);
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(
        3,
        &format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources {resources} /Contents 4 0 R >>"
        ),
    );
    b.stream(4, "", content);
    for &(num, body) in extra {
        b.object(num, body);
    }
    for &(num, dict, data) in streams {
        b.stream(num, dict, data);
    }
    b.build(1)
}

struct Case {
    name: &'static str,
    pdf: Vec<u8>,
    scale: f32,
    tier: GlyphPainting,
    digest: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        // The four device colour spaces in one page, so a change to any of their
        // component-to-RGB conversions moves the digest.
        Case {
            name: "device_colorspaces",
            pdf: page_doc(
                "<< >>",
                b"1 0 0 rg 10 10 40 40 re f \
              0 1 0 RG 4 w 60 10 40 40 re S \
              0.5 g 110 10 40 40 re f \
              0 1 1 0 k 10 60 40 40 re f \
              0.2 0.4 0.6 sc 60 60 40 40 re f",
                &[],
                &[],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "80b2fc103a5bf135",
        },
        // Nested clips: the inner rect is clipped twice, so only the overlap paints.
        // Exercises Mask::intersected and the clip handle surviving a q/Q pair.
        Case {
            name: "nested_clips",
            pdf: page_doc(
                "<< >>",
                b"q 20 20 120 120 re W n \
                q 80 80 100 100 re W n 0 0 1 rg 0 0 200 200 re f Q \
                1 0 0 rg 0 0 60 60 re f \
              Q \
              0 0.5 0 rg 150 150 40 40 re f",
                &[],
                &[],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "a8e8012c1fe4c4e5",
        },
        // A form with /Matrix and /BBox, invoked twice at different CTMs, whose own
        // /Resources names a nested form. Covers the chain prepend, BBox clipping,
        // and that a form's effects do not leak into its caller.
        Case {
            name: "nested_forms",
            pdf: page_doc(
                "<< /XObject << /Outer 10 0 R >> >>",
                b"q 1 0 0 1 0 0 cm /Outer Do Q \
              q 1 0 0 1 90 90 cm /Outer Do Q \
              0 0 0 rg 0 190 10 10 re f",
                &[],
                &[
                    (
                        10,
                        "/Type /XObject /Subtype /Form /BBox [0 0 80 80] \
                     /Matrix [1 0 0 1 10 10] \
                     /Resources << /XObject << /Inner 11 0 R >> >>",
                        b"1 0 0 rg 0 0 100 40 re f q 2 0 0 2 0 40 cm /Inner Do Q",
                    ),
                    (
                        11,
                        "/Type /XObject /Subtype /Form /BBox [0 0 40 40]",
                        b"0 0 1 rg 0 0 20 20 re f",
                    ),
                ],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "3722e377b99ef8b5",
        },
        // An unbalanced `Q` inside a form must not pop the caller's graphics state.
        // The page relies on its own q/Q bracket still holding after the form runs.
        Case {
            name: "form_with_unbalanced_restore",
            pdf: page_doc(
                "<< /XObject << /F 10 0 R >> >>",
                b"q 1 0 0 1 100 0 cm /F Do 0 0 1 rg 0 100 40 40 re f Q \
              1 0 0 rg 0 0 40 40 re f",
                &[],
                &[(
                    10,
                    "/Type /XObject /Subtype /Form /BBox [0 0 200 200]",
                    b"Q Q 0 0.5 0 rg 0 0 40 40 re f",
                )],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "fc8164e1c5054cc5",
        },
        // A path and a pending `W` straddling a `Do`: the clip is set up, a form runs,
        // and the page then paints expecting the clip to be in force.
        Case {
            name: "clip_straddling_a_form",
            pdf: page_doc(
                "<< /XObject << /F 10 0 R >> >>",
                b"20 20 100 100 re W n /F Do 1 0 0 rg 0 0 200 200 re f",
                &[],
                &[(
                    10,
                    "/Type /XObject /Subtype /Form /BBox [0 0 200 200]",
                    b"0 0 1 rg 0 0 200 60 re f",
                )],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "6b78a3cf7cf7fde5",
        },
        // Resource-name shadowing: /X names one form on the page and a different one
        // inside the outer form, so a chain built innermost-first must pick the inner.
        Case {
            name: "form_resource_shadowing",
            pdf: page_doc(
                "<< /XObject << /Outer 10 0 R /X 12 0 R >> >>",
                b"/Outer Do /X Do",
                &[],
                &[
                    (
                        10,
                        "/Type /XObject /Subtype /Form /BBox [0 0 200 200] \
                     /Resources << /XObject << /X 11 0 R >> >>",
                        b"/X Do",
                    ),
                    (
                        11,
                        "/Type /XObject /Subtype /Form /BBox [0 0 200 200]",
                        b"1 0 0 rg 10 10 40 40 re f",
                    ),
                    (
                        12,
                        "/Type /XObject /Subtype /Form /BBox [0 0 200 200]",
                        b"0 0 1 rg 10 60 40 40 re f",
                    ),
                ],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "49c650a0c4c74bc5",
        },
        // Two overlapping `d0` Type3 glyphs, each setting its own colour. They
        // composite last-wins, so reversing glyph order changes the overlap's colour —
        // a pixel hazard, not just a report-ordering one.
        Case {
            name: "type3_overlapping_d0_glyphs",
            pdf: page_doc(
                "<< /Font << /T3 10 0 R >> >>",
                b"BT /T3 100 Tf 20 100 Td <4142> Tj ET",
                &[(
                    10,
                    "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] \
                 /Encoding << /Differences [65 /red 66 /blue] >> \
                 /CharProcs << /red 11 0 R /blue 12 0 R >> \
                 /FirstChar 65 /Widths [300 300] >>",
                )],
                &[
                    (11, "", b"1000 0 d0 1 0 0 rg 0 0 600 600 re f"),
                    (12, "", b"1000 0 d0 0 0 1 rg 0 0 600 600 re f"),
                ],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "2285273f0098bbf5",
        },
        // A `d1` CharProc painting through a form XObject. `d1` is uncoloured, so the
        // glyph must paint in the inherited text fill; the form's own `0 0 1 rg` is
        // ignored. Whether the colour lock reaches into a nested form frame is
        // exactly what a per-frame flag can silently get wrong.
        Case {
            name: "type3_d1_painting_via_a_form",
            pdf: page_doc(
                "<< /Font << /T3 10 0 R >> >>",
                b"1 0 0 rg BT /T3 100 Tf 20 60 Td <41> Tj ET",
                &[(
                    10,
                    "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] \
                 /Encoding << /Differences [65 /boxglyph] >> \
                 /CharProcs << /boxglyph 11 0 R >> /FirstChar 65 /Widths [1000] \
                 /Resources << /XObject << /Fx 12 0 R >> >> >>",
                )],
                &[
                    (11, "", b"1000 0 0 0 1000 1000 d1 /Fx Do"),
                    (
                        12,
                        "/Type /XObject /Subtype /Form /BBox [0 0 1000 1000]",
                        b"0 0 1 rg 100 0 500 700 re f",
                    ),
                ],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "29fceaec09ce08dd",
        },
        // A Type3 show-string longer than the form-depth cap. Every glyph must paint:
        // a stack that batch-pushes glyph frames would exhaust the cap partway and
        // silently drop the tail.
        Case {
            name: "type3_twenty_glyph_string",
            pdf: page_doc(
                "<< /Font << /T3 10 0 R >> >>",
                b"BT /T3 20 Tf 5 100 Td <4141414141414141414141414141414141414141> Tj ET",
                &[(
                    10,
                    "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] \
                 /Encoding << /Differences [65 /boxglyph] >> \
                 /CharProcs << /boxglyph 11 0 R >> /FirstChar 65 /Widths [500] >>",
                )],
                &[(11, "", b"500 0 d0 0 0 0 rg 0 0 400 900 re f")],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "0ff8d2c19ebe2e85",
        },
        // An 8-bit /Indexed image over /DeviceRGB, drawn under a scaling CTM so the
        // sampler is exercised, plus an /ImageMask stencil painting in the fill colour.
        Case {
            name: "indexed_image_and_stencil",
            pdf: page_doc(
                "<< /XObject << /Img 10 0 R /Stencil 11 0 R >> >>",
                b"q 100 0 0 100 10 90 cm /Img Do Q \
              0 0.6 0 rg q 60 0 0 60 120 20 cm /Stencil Do Q",
                &[],
                &[
                    (
                        10,
                        "/Type /XObject /Subtype /Image /Width 4 /Height 4 \
                     /BitsPerComponent 8 \
                     /ColorSpace [/Indexed /DeviceRGB 3 <FF0000 00FF00 0000FF 000000>]",
                        &[
                            0, 1, 2, 3, //
                            1, 2, 3, 0, //
                            2, 3, 0, 1, //
                            3, 0, 1, 2,
                        ],
                    ),
                    (
                        11,
                        "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                     /BitsPerComponent 1 /ImageMask true",
                        &[
                            0b1010_1010,
                            0b0101_0101,
                            0b1100_0011,
                            0b0011_1100,
                            0b1111_0000,
                            0b0000_1111,
                            0b1000_0001,
                            0b0111_1110,
                        ],
                    ),
                ],
            ),
            scale: 1.0,
            tier: GlyphPainting::AllEmbedded,
            digest: "67248a06ded0546f",
        },
        // The committed fixture with real graphics, at a non-integer scale so the
        // device-space geometry is not a whole number of pixels.
        Case {
            name: "shapes_fixture_scale_1_5",
            pdf: std::fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/shapes.pdf"
            ))
            .expect("committed fixture"),
            scale: 1.5,
            tier: GlyphPainting::AllEmbedded,
            digest: "5c2fc9c7b83a198f",
        },
    ]
}

/// Renders one case's first page.
fn render(case: &Case) -> Pixmap {
    let doc = Document::load(case.pdf.clone()).expect("fixture must load");
    let page = doc.page(0).expect("fixture must have a page");
    let opts = RenderOptions {
        glyph_painting: case.tier,
        ..RenderOptions::default()
    };
    render_page_with_options(&doc, &page, case.scale, &opts).expect("render must succeed")
}

#[test]
fn rendered_pages_match_their_recorded_digests() {
    let mut moved = Vec::new();
    for case in cases() {
        let pix = render(&case);
        let got = digest(&pix);
        if got != case.digest {
            moved.push(format!(
                "  {:<32} {}x{} ink={} digest={}  (recorded {})",
                case.name,
                pix.width,
                pix.height,
                ink(&pix),
                got,
                if case.digest.is_empty() {
                    "<none>"
                } else {
                    case.digest
                },
            ));
        }
    }
    assert!(
        moved.is_empty(),
        "the render changed for {} case(s):\n{}\n\n\
         If this was intended, record the new digest in the same commit that changes \
         the output and say why in its message. If it was not, the rewrite moved pixels.",
        moved.len(),
        moved.join("\n"),
    );
}

/// The digests must actually discriminate. A harness whose cases all render blank
/// would pass every future rewrite while proving nothing, and a blank page is
/// exactly what leniency produces when a fixture is malformed — so each case has
/// to put ink on the page, and no two cases may render identically.
#[test]
fn every_case_paints_something_distinct() {
    let mut seen: Vec<(String, &'static str)> = Vec::new();
    for case in cases() {
        let pix = render(&case);
        let inked = pix
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] != 255 || p[1] != 255 || p[2] != 255)
            .count();
        assert!(
            inked > 0,
            "case {} rendered blank, so its digest proves nothing",
            case.name
        );
        let d = digest(&pix);
        if let Some((_, other)) = seen.iter().find(|(prev, _)| *prev == d) {
            panic!("cases {} and {} render identically", case.name, other);
        }
        seen.push((d, case.name));
    }
}

/// Pixels of exactly this colour. Every fixture that uses this paints
/// axis-aligned rectangles at scale 1, so there is no anti-aliasing and the
/// counts are exact rather than approximate.
fn count_rgb(pix: &Pixmap, rgb: [u8; 3]) -> usize {
    pix.data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] == rgb[0] && p[1] == rgb[1] && p[2] == rgb[2])
        .count()
}

fn case(name: &str) -> Pixmap {
    let found = cases().into_iter().find(|c| c.name == name);
    render(&found.expect("no such case"))
}

/// A `d1` (uncoloured) Type3 glyph paints in the **inherited text fill** even when
/// the painting happens inside a nested form XObject: ISO 32000-1 9.6.5.2 says such
/// a glyph "shall not specify any colour", and the lock that enforces that reaches
/// through the `Do`.
///
/// This is the behaviour the work-stack rewrite is most likely to break silently.
/// The lock is written as a save/restore bracket around the CharProc's nested run
/// and read once in the colour operators; the form re-entry never touches it, which
/// is *why* a form frame inherits it. Reimplemented as a plain per-frame flag it
/// needs two different rules — copy the parent's value on a form push, set it from
/// `d0`/`d1` on a CharProc push — and getting that wrong turns this page blue with
/// every other render test still green.
#[test]
fn a_d1_char_proc_keeps_the_text_fill_inside_a_nested_form() {
    let pix = case("type3_d1_painting_via_a_form");
    assert_eq!(
        count_rgb(&pix, [255, 0, 0]),
        3500,
        "the glyph must paint in the text fill red set before the BT"
    );
    assert_eq!(
        count_rgb(&pix, [0, 0, 255]),
        0,
        "the form's own `0 0 1 rg` must be ignored inside a d1 glyph"
    );
}

/// Two overlapping `d0` (coloured) Type3 glyphs composite in string order, so the
/// second one wins where they overlap.
///
/// Both glyphs are 60 units wide and advance 30, so the second covers half the
/// first. Reversing the order — which a stack that pushes a batch of glyph frames
/// does for free — would swap these two counts while leaving total ink identical.
#[test]
fn overlapping_d0_glyphs_composite_in_string_order() {
    let pix = case("type3_overlapping_d0_glyphs");
    assert_eq!(
        count_rgb(&pix, [0, 0, 255]),
        3600,
        "the second glyph paints whole, over the first"
    );
    assert_eq!(
        count_rgb(&pix, [255, 0, 0]),
        1800,
        "the first glyph survives only where the second does not cover it"
    );
}

/// Every glyph of a Type3 show-string paints, however long the string is.
///
/// The form-recursion depth cap bounds a self-referential glyph, and a CharProc
/// costs one level of it. If glyph frames were pushed as a batch rather than one at
/// a time, the cap would count them as nesting instead: with a cap of 16, this
/// twenty-glyph string would lose its tail and no other test would notice.
#[test]
fn every_glyph_of_a_long_type3_string_paints() {
    let pix = case("type3_twenty_glyph_string");
    for i in 0..20u32 {
        let x = 5 + 10 * i + 3;
        let o = ((90 * pix.width + x) * 4) as usize;
        assert!(
            pix.data[o] < 128 && pix.data[o + 1] < 128 && pix.data[o + 2] < 128,
            "glyph {i} of 20 did not paint at x={x}"
        );
    }
}

/// An asynchronous source that answers everything with `null`.
///
/// The heap field is load-bearing: rustc const-promotes a reference to a unit
/// struct to `&'static`, so a unit stub would satisfy the `'static` assertion
/// below even under a by-reference signature the assertion exists to reject.
/// A `Vec` cannot be promoted. It is `Send + Sync` because the executor
/// borrows its source across awaits, so the owning future is `Send` only when
/// the source is `Sync` — as every genuinely asynchronous source already is.
struct NullSource {
    payload: Vec<u8>,
}

impl pdfboss_core::AsyncObjectSource for NullSource {
    fn get(&self, _r: pdfboss_core::ObjRef) -> pdfboss_core::BoxFuture<'_, PdfResult<Object>> {
        Box::pin(std::future::ready(Ok(Object::Null)))
    }

    fn stream_data<'a>(
        &'a self,
        _s: &'a pdfboss_core::Stream,
    ) -> pdfboss_core::BoxFuture<'a, PdfResult<Vec<u8>>> {
        Box::pin(std::future::ready(Ok(self.payload.clone())))
    }

    fn resolve<'a>(&'a self, o: &'a Object) -> pdfboss_core::BoxFuture<'a, PdfResult<Object>> {
        Box::pin(pdfboss_core::resolve_with(self, o))
    }
}

use pdfboss_core::{Object, Result as PdfResult};
use pdfboss_render::render_page_reporting_with;

/// The async render entry point must produce a future a runtime's `spawn`
/// and the Python bindings will both accept: `Send + 'static`.
///
/// The `async move` block is the shape a consumer actually writes — it owns
/// the source and the page, and the borrow `render_page_reporting_with`
/// takes is created inside it. The document is dropped first to show the
/// page stands alone. Both halves of the gate were verified to bite:
/// borrowing the page from outside the block fails with `E0597 ... borrowed
/// for 'static`, and substituting `Immediate<Document>` (owned, so
/// `'static`, but `!Sync` through its object cache) fails with `E0277
/// Rc<Object> cannot be sent between threads safely`.
#[test]
fn the_async_render_entry_point_yields_a_spawnable_future() {
    fn assert_send_static<F: std::future::Future + Send + 'static>(_: &F) {}

    let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
    let page = doc.page(0).unwrap();
    drop(doc);

    let fut = async move {
        render_page_reporting_with(
            NullSource {
                payload: Vec::new(),
            },
            &page,
            1.0,
            &RenderOptions::default(),
        )
        .await
    };
    assert_send_static(&fut);

    // A source that resolves everything to null yields a page with empty
    // contents: a blank pixmap sized from the page, and nothing reported.
    // Driving it proves the wiring is reachable; the type is the gate.
    let (pix, report) = pdfboss_core::block_on(fut).expect("render");
    assert_eq!((pix.width, pix.height), (612, 792));
    assert!(report.is_empty(), "a null page has nothing to report");
}
