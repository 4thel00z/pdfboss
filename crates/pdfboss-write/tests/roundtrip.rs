//! Oracle round-trip suite: documents built with `pdfboss-write` are read
//! back through `pdfboss-core`, extracted with `pdfboss-output` and
//! rasterized with `pdfboss-render`, so the writer is verified against the
//! toolkit's own readers rather than against expected byte dumps.

use pdfboss_core::object::decode_text_string;
use pdfboss_core::{Dict, Document, Matrix, Name, Object, Rect, Stream};
use pdfboss_output::{extract_text, ReadingOrder};
use pdfboss_render::{render_page_reporting, RenderOptions};
use pdfboss_write::{
    Attachment, BlendMode, Bookmark, Canvas, Color, Content, Date, Error, ImageData, LabelStyle,
    Link, LinkAnnotation, LinkTarget, Metadata, Outline, Page, PageLabel, PageLayout, PageMode,
    PageSize, Paragraph, Pdf, Standard14, Viewer, WriteOptions, XrefStyle,
};

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn hello_single_page_round_trips() {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("Hello, world!", 72.0, 770.0, Standard14::Helvetica, 24.0)
        .unwrap();
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    assert_eq!(doc.page_count(), 1);
    let loaded = doc.page(0).unwrap();
    assert_eq!(loaded.media_box, Rect::new(0.0, 0.0, 595.28, 841.89));
    let text = extract_text(&doc, &loaded, ReadingOrder::Content).unwrap();
    assert!(text.contains("Hello, world!"), "extracted: {text:?}");
}

#[test]
fn metadata_and_multipage_round_trip() {
    let mut second = Page::new(PageSize::A5);
    second.rotation = 90;
    let pdf = Pdf {
        metadata: Some(Metadata {
            title: Some("Résumé".into()),
            author: Some("pdfboss".into()),
            creation_date: Some(Date {
                year: 2026,
                month: 8,
                day: 27,
                hour: 12,
                minute: 30,
                second: 15,
                utc_offset_minutes: 0,
            }),
            ..Metadata::default()
        }),
        pages: vec![Page::new(PageSize::A4), second, Page::new(PageSize::Letter)],
        outline: None,
        attachments: Vec::new(),
        page_labels: Vec::new(),
        viewer: None,
        options: WriteOptions::default(),
    };
    let doc = Document::load(pdf.to_bytes().unwrap()).unwrap();
    assert_eq!(doc.page_count(), 3);
    let meta = doc.metadata();
    assert_eq!(meta.title.as_deref(), Some("Résumé"));
    assert_eq!(meta.author.as_deref(), Some("pdfboss"));
    assert_eq!(meta.creation_date.as_deref(), Some("D:20260827123015Z"));
    let first = doc.page(0).unwrap();
    assert_eq!(first.media_box, Rect::new(0.0, 0.0, 595.28, 841.89));
    assert_eq!(first.rotate, 0);
    let rotated = doc.page(1).unwrap();
    assert_eq!(rotated.media_box, Rect::new(0.0, 0.0, 419.53, 595.28));
    assert_eq!(rotated.rotate, 90);
    assert_eq!(
        doc.page(2).unwrap().media_box,
        Rect::new(0.0, 0.0, 612.0, 792.0)
    );
}

#[test]
fn all_none_metadata_writes_no_info() {
    let pdf = Pdf {
        metadata: Some(Metadata::default()),
        pages: vec![Page::new(PageSize::A4)],
        ..Pdf::default()
    };
    let bytes = pdf.to_bytes().unwrap();
    assert!(!contains(&bytes, b"/Info"));
    let doc = Document::load(bytes).unwrap();
    assert_eq!(doc.metadata(), pdfboss_core::document::Metadata::default());
}

#[test]
fn non_quarter_rotation_is_an_error() {
    let mut page = Page::new(PageSize::A4);
    page.rotation = 45;
    let outcome = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes();
    match outcome {
        Err(Error::Other(msg)) => assert!(msg.contains("45"), "{msg}"),
        other => panic!("expected Error::Other naming the rotation, got {other:?}"),
    }
}

#[test]
fn shapes_and_image_render_without_skips() {
    let mut page = Page::new(PageSize::Custom {
        width: 200.0,
        height: 200.0,
    });
    page.canvas.set_fill(Color::Rgb(1.0, 0.0, 0.0));
    page.canvas.rect(20.0, 20.0, 60.0, 60.0);
    page.canvas.fill();
    let gradient: Vec<u8> = (0..256).map(|value| value as u8).collect();
    let handle = page
        .canvas
        .add_image(ImageData::gray8(16, 16, gradient).unwrap());
    page.canvas.draw_image(handle, 120.0, 120.0, 60.0, 60.0);
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let (pix, report) =
        render_page_reporting(&doc, &loaded, 1.0, &RenderOptions::default()).unwrap();
    assert!(report.is_empty(), "skipped content: {:?}", report.summary());
    assert_eq!((pix.width, pix.height), (200, 200));
    let sample = |x: usize, y: usize| {
        let at = (y * pix.width as usize + x) * 4;
        (pix.data[at], pix.data[at + 1], pix.data[at + 2])
    };
    assert_eq!(sample(5, 5), (255, 255, 255));
    assert_eq!(sample(100, 100), (255, 255, 255));
    let (r, g, b) = sample(50, 150);
    assert!(
        r >= 250 && g <= 5 && b <= 5,
        "rect region painted ({r}, {g}, {b}), expected red"
    );
    let (ir, ig, ib) = sample(150, 50);
    assert!(ir < 250, "image region stayed white: ({ir}, {ig}, {ib})");
}

fn tiny_jpeg(width: u16, height: u16) -> Vec<u8> {
    let components = 3u8;
    let mut out: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xC0];
    out.extend_from_slice(&(8 + 3 * u16::from(components)).to_be_bytes());
    out.push(8);
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.push(components);
    for id in 0..components {
        out.extend_from_slice(&[id + 1, 0x11, 0]);
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

#[test]
fn jpeg_passthrough_keeps_dct_stream() {
    let jpeg = tiny_jpeg(5, 7);
    let mut page = Page::new(PageSize::A4);
    let handle = page.canvas.add_image(ImageData::jpeg(&jpeg).unwrap());
    page.canvas.draw_image(handle, 100.0, 100.0, 50.0, 70.0);
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let xobjects = doc
        .resolve(loaded.resources.get("XObject").expect("XObject resource"))
        .unwrap();
    let entry = xobjects.as_dict().unwrap().get("Im1").expect("Im1 entry");
    let resolved = doc.resolve(entry).unwrap();
    let stream = resolved.as_stream().unwrap();
    assert_eq!(
        stream.dict.get_name("Filter"),
        Some(&Name("DCTDecode".into()))
    );
    assert_eq!(stream.dict.get_int("Width"), Some(5));
    assert_eq!(stream.dict.get_int("Height"), Some(7));
    assert_eq!(stream.data, jpeg);
}

fn letterhead() -> Canvas {
    let mut canvas = Canvas::new();
    canvas.set_fill(Color::Gray(0.9));
    canvas.rect(0.0, 0.0, 200.0, 40.0);
    canvas.fill();
    canvas
        .text("Letterhead", 5.0, 10.0, Standard14::Helvetica, 12.0)
        .unwrap();
    canvas
}

/// Resolves the page/form resource named `resource_name` under `/XObject`
/// to its stream, cloned so the borrow does not outlive a temporary
/// `Document::resolve` result.
fn resolve_form(doc: &Document, resources: &Dict, resource_name: &str) -> Stream {
    let xobjects = resources
        .get_dict("XObject")
        .expect("resources carry an XObject dict");
    let entry = xobjects
        .get(resource_name)
        .unwrap_or_else(|| panic!("XObject resource {resource_name} present"));
    doc.resolve(entry)
        .expect("form reference resolves")
        .as_stream()
        .expect("form XObject is a stream")
        .clone()
}

#[test]
fn draw_group_paints_the_registered_subcanvas() {
    let mut page = Page::new(PageSize::A4);
    let handle = page.canvas.group(letterhead(), [0.0, 0.0, 200.0, 40.0]);
    page.canvas
        .draw_group(handle, Matrix::translate(50.0, 700.0));
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let text = extract_text(&doc, &loaded, ReadingOrder::Content).unwrap();
    assert!(text.contains("Letterhead"), "extracted: {text:?}");
    let form = resolve_form(&doc, &loaded.resources, "Gp1");
    assert_eq!(form.dict.get_name("Type"), Some(&Name("XObject".into())));
    assert_eq!(form.dict.get_name("Subtype"), Some(&Name("Form".into())));
    assert_eq!(bbox_values(&form.dict), [0.0, 0.0, 200.0, 40.0]);
}

/// A form's `/BBox` as four `f64`s. Integral values round-trip through the
/// object parser as `Object::Int`, not `Object::Real` (the crate's
/// documented integral-`Real` corner), so numeric comparison goes through
/// `as_f64` rather than comparing `Object` variants directly.
fn bbox_values(dict: &Dict) -> [f64; 4] {
    let array = dict.get_array("BBox").expect("BBox present");
    let mut values = [0.0; 4];
    for (slot, value) in values.iter_mut().zip(array) {
        *slot = value.as_f64().expect("BBox entry is numeric");
    }
    values
}

/// Two pages each register their own copy of the same letterhead canvas.
/// Cross-page form reuse is not implemented (groups live per-canvas, and a
/// `Canvas` cannot be shared across pages) — this asserts the two resulting
/// forms are structurally equal instead, and that the document-wide font
/// cache is still shared between them.
#[test]
fn letterhead_group_on_two_pages_yields_structurally_equal_forms() {
    let mut first = Page::new(PageSize::A4);
    let first_handle = first.canvas.group(letterhead(), [0.0, 0.0, 200.0, 40.0]);
    first
        .canvas
        .draw_group(first_handle, Matrix::translate(50.0, 700.0));
    let mut second = Page::new(PageSize::A4);
    let second_handle = second.canvas.group(letterhead(), [0.0, 0.0, 200.0, 40.0]);
    second
        .canvas
        .draw_group(second_handle, Matrix::translate(50.0, 700.0));
    let bytes = Pdf {
        pages: vec![first, second],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let page0 = doc.page(0).unwrap();
    let page1 = doc.page(1).unwrap();
    assert!(extract_text(&doc, &page0, ReadingOrder::Content)
        .unwrap()
        .contains("Letterhead"));
    assert!(extract_text(&doc, &page1, ReadingOrder::Content)
        .unwrap()
        .contains("Letterhead"));

    let form0 = resolve_form(&doc, &page0.resources, "Gp1");
    let form1 = resolve_form(&doc, &page1.resources, "Gp1");
    assert_eq!(form0.dict.get_array("BBox"), form1.dict.get_array("BBox"));
    assert_eq!(
        doc.stream_data(&form0).unwrap(),
        doc.stream_data(&form1).unwrap(),
        "identical sub-canvases must serialize to identical form content"
    );

    let font0 = form0
        .dict
        .get_dict("Resources")
        .and_then(|r| r.get_dict("Font"))
        .and_then(|f| f.get_ref("F1"))
        .expect("form0 references F1");
    let font1 = form1
        .dict
        .get_dict("Resources")
        .and_then(|r| r.get_dict("Font"))
        .and_then(|f| f.get_ref("F1"))
        .expect("form1 references F1");
    assert_eq!(
        font0, font1,
        "the document-wide font cache must be shared with nested forms"
    );

    let gp0 = page0
        .resources
        .get_dict("XObject")
        .and_then(|x| x.get_ref("Gp1"))
        .expect("page0 Gp1 ref");
    let gp1 = page1
        .resources
        .get_dict("XObject")
        .and_then(|x| x.get_ref("Gp1"))
        .expect("page1 Gp1 ref");
    assert_ne!(
        gp0, gp1,
        "cross-page group reuse is deferred: each page gets its own form object"
    );
}

#[test]
fn drawing_one_group_twice_shares_the_same_form_xobject() {
    let mut page = Page::new(PageSize::A4);
    let handle = page.canvas.group(letterhead(), [0.0, 0.0, 200.0, 40.0]);
    page.canvas
        .draw_group(handle, Matrix::translate(10.0, 10.0));
    page.canvas
        .draw_group(handle, Matrix::translate(10.0, 400.0));
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let xobjects = loaded
        .resources
        .get_dict("XObject")
        .expect("XObject resource dict");
    assert_eq!(
        xobjects.len(),
        1,
        "two draw_group calls on one handle must reference one form"
    );
    let text = extract_text(&doc, &loaded, ReadingOrder::Content).unwrap();
    assert_eq!(
        text.matches("Letterhead").count(),
        2,
        "the shared form paints once per draw_group call"
    );
}

#[test]
fn nested_groups_recurse_through_extraction() {
    let mut inner = Canvas::new();
    inner
        .text("Inner", 2.0, 2.0, Standard14::Helvetica, 8.0)
        .unwrap();
    let mut outer = Canvas::new();
    let inner_handle = outer.group(inner, [0.0, 0.0, 40.0, 12.0]);
    outer.draw_group(inner_handle, Matrix::identity());
    let mut page = Page::new(PageSize::A4);
    let outer_handle = page.canvas.group(outer, [0.0, 0.0, 40.0, 12.0]);
    page.canvas
        .draw_group(outer_handle, Matrix::translate(100.0, 500.0));
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let text = extract_text(&doc, &loaded, ReadingOrder::Content).unwrap();
    assert!(text.contains("Inner"), "extracted: {text:?}");
}

#[test]
fn fill_alpha_resolves_to_ca_in_extgstate() {
    let mut page = Page::new(PageSize::A4);
    page.canvas.set_fill_alpha(0.5);
    page.canvas.rect(0.0, 0.0, 10.0, 10.0);
    page.canvas.fill();
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let ext_gstates = loaded
        .resources
        .get_dict("ExtGState")
        .expect("ExtGState resource dict");
    let gs1_ref = ext_gstates.get_ref("Gs1").expect("Gs1 entry");
    let gs1 = resolve_dict(&doc, &Object::Ref(gs1_ref));
    assert_eq!(gs1.get_f64("ca"), Some(0.5));
    assert!(gs1.get("CA").is_none());
    assert!(gs1.get("BM").is_none());
}

#[test]
fn stroke_alpha_and_blend_mode_resolve_to_distinct_extgstates() {
    let mut page = Page::new(PageSize::A4);
    page.canvas.set_stroke_alpha(0.75);
    page.canvas.set_blend_mode(BlendMode::Multiply);
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let ext_gstates = loaded
        .resources
        .get_dict("ExtGState")
        .expect("ExtGState resource dict");
    assert_eq!(ext_gstates.len(), 2);
    let gs1 = resolve_dict(
        &doc,
        &Object::Ref(ext_gstates.get_ref("Gs1").expect("Gs1 entry")),
    );
    assert_eq!(gs1.get_f64("CA"), Some(0.75));
    assert!(gs1.get("ca").is_none());
    let gs2 = resolve_dict(
        &doc,
        &Object::Ref(ext_gstates.get_ref("Gs2").expect("Gs2 entry")),
    );
    assert_eq!(gs2.get_name("BM"), Some(&Name("Multiply".into())));
}

fn two_page_document() -> Pdf {
    let mut first = Page::new(PageSize::A4);
    first
        .canvas
        .text("Determinism", 72.0, 700.0, Standard14::TimesRoman, 14.0)
        .unwrap();
    first.canvas.set_fill(Color::Gray(0.25));
    first.canvas.rect(10.0, 10.0, 40.0, 40.0);
    first.canvas.fill();
    let mut second = Page::new(PageSize::A5.landscape());
    let raster: Vec<u8> = (0..16u8).map(|value| value * 16).collect();
    let handle = second
        .canvas
        .add_image(ImageData::gray8(4, 4, raster).unwrap());
    second.canvas.draw_image(handle, 20.0, 20.0, 80.0, 80.0);
    second
        .canvas
        .text("Page two", 30.0, 200.0, Standard14::Helvetica, 12.0)
        .unwrap();
    Pdf {
        metadata: Some(Metadata {
            title: Some("Determinism".into()),
            ..Metadata::default()
        }),
        pages: vec![first, second],
        outline: None,
        attachments: Vec::new(),
        page_labels: Vec::new(),
        viewer: None,
        options: WriteOptions::default(),
    }
}

#[test]
fn identical_documents_serialize_byte_identically() {
    assert_eq!(
        two_page_document().to_bytes().unwrap(),
        two_page_document().to_bytes().unwrap()
    );
}

#[test]
fn both_xref_styles_load() {
    for xref in [XrefStyle::Table, XrefStyle::Stream] {
        let mut page = Page::new(PageSize::A4);
        page.canvas
            .text("Xref", 72.0, 700.0, Standard14::Courier, 10.0)
            .unwrap();
        let bytes = Pdf {
            pages: vec![page],
            options: WriteOptions {
                xref,
                ..WriteOptions::default()
            },
            ..Pdf::default()
        }
        .to_bytes()
        .unwrap();
        match xref {
            XrefStyle::Table => {
                assert!(contains(&bytes, b"\nxref\n"));
                assert!(contains(&bytes, b"trailer"));
                assert!(!contains(&bytes, b"/ObjStm"));
            }
            XrefStyle::Stream => assert!(contains(&bytes, b"/ObjStm")),
        }
        let doc = Document::load(bytes).unwrap();
        assert_eq!(doc.page_count(), 1);
        let loaded = doc.page(0).unwrap();
        assert!(extract_text(&doc, &loaded, ReadingOrder::Content)
            .unwrap()
            .contains("Xref"));
    }
}

#[test]
fn link_annotations_round_trip() {
    let mut page = Page::new(PageSize::A4);
    page.canvas
        .text("pdfboss", 72.0, 700.0, Standard14::Helvetica, 12.0)
        .unwrap();
    page.links.push(LinkAnnotation {
        rect: [72.0, 697.0, 130.0, 712.0],
        target: LinkTarget::Uri("https://example.com/docs".to_string()),
    });
    let bytes = Pdf {
        pages: vec![page],
        options: WriteOptions {
            xref: XrefStyle::Table,
            ..WriteOptions::default()
        },
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    assert!(contains(&bytes, b"/Annots"));
    assert!(contains(&bytes, b"/Link"));
    assert!(contains(&bytes, b"https://example.com/docs"));
    let doc = Document::load(bytes).unwrap();
    assert_eq!(doc.page_count(), 1);
}

#[test]
fn goto_links_resolve_to_their_page() {
    let mut first = Page::new(PageSize::A4);
    first
        .canvas
        .text("to appendix", 72.0, 700.0, Standard14::Helvetica, 12.0)
        .unwrap();
    first.links.push(LinkAnnotation {
        rect: [72.0, 697.0, 150.0, 712.0],
        target: LinkTarget::Page(1),
    });
    let second = Page::new(PageSize::A4);
    let bytes = Pdf {
        pages: vec![first, second],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    assert_eq!(doc.page_count(), 2);
    let page = doc.page(0).unwrap();
    let annots = page.dict().get_array("Annots").unwrap_or(&[]);
    let mut resolved_page_types: Vec<String> = Vec::new();
    for annot in annots {
        let annot = doc.resolve(annot).unwrap();
        let annot = annot.as_dict().unwrap();
        let action = annot.get_dict("A").unwrap();
        let subtype = action.get("S").unwrap().as_name().unwrap();
        assert_eq!(subtype.0, "GoTo");
        let destination = action.get_array("D").unwrap();
        let target = doc.resolve(&destination[0]).unwrap();
        let target = target.as_dict().unwrap();
        let page_type = target.get("Type").unwrap().as_name().unwrap();
        resolved_page_types.push(page_type.0.clone());
    }
    assert_eq!(resolved_page_types, vec!["Page".to_string()]);
}

#[test]
fn link_element_lands_in_page_links() {
    let mut page = Page::new(PageSize::A4);
    page.content.push(Content::from(Link {
        rect: [10.0, 10.0, 60.0, 24.0],
        target: LinkTarget::Uri("https://example.com".into()),
    }));
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let annots = loaded.dict().get_array("Annots").unwrap_or(&[]);
    assert_eq!(annots.len(), 1);
    let annot = doc.resolve(&annots[0]).unwrap();
    let annot = annot.as_dict().unwrap();
    assert_eq!(annot.get_name("Subtype"), Some(&Name("Link".into())));
    let action = annot.get_dict("A").unwrap();
    assert_eq!(action.get_name("S"), Some(&Name("URI".into())));
    assert_eq!(
        action.get("URI").unwrap().as_str_bytes(),
        Some(b"https://example.com".as_slice())
    );
}

#[test]
fn paragraph_wraps_and_extracts_across_lines() {
    let mut page = Page::new(PageSize::A4);
    page.content.push(Content::from(Paragraph {
        text: "aaaaaaaaa bbbbbbbbbb cccccccccc".into(),
        rect: [72.0, 680.0, 192.0, 780.0],
        font: Standard14::Courier,
        size: 10.0,
        ..Paragraph::default()
    }));
    let bytes = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let loaded = doc.page(0).unwrap();
    let text = extract_text(&doc, &loaded, ReadingOrder::Content).unwrap();
    assert!(
        text.contains("aaaaaaaaa bbbbbbbbbb"),
        "expected the first wrapped line intact, got {text:?}"
    );
    assert!(
        text.contains("cccccccccc"),
        "expected the second wrapped line intact, got {text:?}"
    );
    let first_line_pos = text.find("aaaaaaaaa").expect("first line present");
    let second_line_pos = text.find("cccccccccc").expect("second line present");
    assert!(
        first_line_pos < second_line_pos,
        "wrapped lines should extract in top-to-bottom order: {text:?}"
    );
}

#[test]
fn goto_link_out_of_range_errors() {
    let mut page = Page::new(PageSize::A4);
    page.links.push(LinkAnnotation {
        rect: [0.0, 0.0, 1.0, 1.0],
        target: LinkTarget::Page(7),
    });
    let err = Pdf {
        pages: vec![page],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap_err();
    assert!(err.to_string().contains("out of range"));
}

/// Resolves an object to its dictionary, cloned so the borrow does not
/// outlive a temporary `Document::resolve` result.
fn resolve_dict(doc: &Document, obj: &Object) -> Dict {
    doc.resolve(obj)
        .expect("reference resolves")
        .as_dict()
        .expect("resolved object is a dictionary")
        .clone()
}

/// The catalog dictionary reached from the trailer's `/Root`.
fn catalog(doc: &Document) -> Dict {
    let root = doc
        .xref()
        .trailer
        .get("Root")
        .expect("/Root present")
        .clone();
    resolve_dict(doc, &root)
}

/// Resolves an object to its stream, cloned so the borrow does not outlive
/// a temporary `Document::resolve` result.
fn resolve_stream(doc: &Document, obj: &Object) -> Stream {
    doc.resolve(obj)
        .expect("reference resolves")
        .as_stream()
        .expect("resolved object is a stream")
        .clone()
}

/// The catalog's `/Metadata` stream, decoded, or `None` when the catalog
/// carries no such entry.
fn xmp_packet(doc: &Document) -> Option<String> {
    let entry = catalog(doc).get("Metadata")?.clone();
    let stream = resolve_stream(doc, &entry);
    assert_eq!(stream.dict.get_name("Type"), Some(&Name("Metadata".into())));
    assert_eq!(stream.dict.get_name("Subtype"), Some(&Name("XML".into())));
    assert!(
        stream.dict.get("Filter").is_none(),
        "the XMP stream must stay uncompressed regardless of WriteOptions::compress"
    );
    let bytes = doc.stream_data(&stream).expect("XMP stream decodes");
    Some(String::from_utf8(bytes).expect("XMP packet is valid UTF-8"))
}

#[test]
fn xmp_packet_maps_full_metadata_and_stays_uncompressed() {
    let pdf = Pdf {
        metadata: Some(Metadata {
            title: Some("Q3 & Q4 Report".into()),
            author: Some("Jane Doe".into()),
            subject: Some("Quarterly numbers".into()),
            keywords: Some("finance, quarterly".into()),
            creator: Some("pdfboss".into()),
            producer: Some("pdfboss-write".into()),
            creation_date: Some(Date {
                year: 2026,
                month: 8,
                day: 27,
                hour: 12,
                minute: 30,
                second: 15,
                utc_offset_minutes: 0,
            }),
            modification_date: Some(Date {
                year: 2026,
                month: 8,
                day: 28,
                hour: 9,
                minute: 0,
                second: 0,
                utc_offset_minutes: 120,
            }),
        }),
        pages: vec![Page::new(PageSize::A4)],
        options: WriteOptions {
            compress: true,
            ..WriteOptions::default()
        },
        ..Pdf::default()
    };
    let doc = Document::load(pdf.to_bytes().unwrap()).unwrap();
    let xml = xmp_packet(&doc).expect("/Metadata present when metadata is Some");
    assert!(xml.contains("Q3 &amp; Q4 Report"), "{xml}");
    assert!(xml.contains("Jane Doe"), "{xml}");
    assert!(xml.contains("pdfboss-write"), "{xml}");
    assert!(xml.contains("2026-08-27T12:30:15Z"), "{xml}");
    assert!(xml.contains("2026-08-28T09:00:00+02:00"), "{xml}");
    assert!(!xml.contains("InstanceID"), "{xml}");
    assert!(!xml.contains("DocumentID"), "{xml}");
    let meta = doc.metadata();
    assert_eq!(meta.title.as_deref(), Some("Q3 & Q4 Report"));
    assert_eq!(meta.author.as_deref(), Some("Jane Doe"));
}

#[test]
fn xmp_packet_escapes_all_five_special_characters() {
    let pdf = Pdf {
        metadata: Some(Metadata {
            title: Some("<&>\"'".into()),
            ..Metadata::default()
        }),
        pages: vec![Page::new(PageSize::A4)],
        ..Pdf::default()
    };
    let doc = Document::load(pdf.to_bytes().unwrap()).unwrap();
    let xml = xmp_packet(&doc).expect("/Metadata present when metadata is Some");
    assert!(xml.contains("&lt;&amp;&gt;&quot;&apos;"), "{xml}");
    assert!(!xml.contains("<&>\"'"), "{xml}");
}

#[test]
fn xmp_packet_is_deterministic_across_builds() {
    fn build() -> Vec<u8> {
        Pdf {
            metadata: Some(Metadata {
                title: Some("Determinism".into()),
                creation_date: Some(Date {
                    year: 2026,
                    month: 8,
                    day: 27,
                    hour: 0,
                    minute: 0,
                    second: 0,
                    utc_offset_minutes: 0,
                }),
                ..Metadata::default()
            }),
            pages: vec![Page::new(PageSize::A4)],
            ..Pdf::default()
        }
        .to_bytes()
        .unwrap()
    }
    let first = build();
    let second = build();
    assert_eq!(first, second);
    let doc = Document::load(first).unwrap();
    let xml = xmp_packet(&doc).expect("/Metadata present when metadata is Some");
    assert!(xml.contains("Determinism"), "{xml}");
}

#[test]
fn no_metadata_writes_no_xmp_stream() {
    let pdf = Pdf {
        metadata: None,
        pages: vec![Page::new(PageSize::A4)],
        ..Pdf::default()
    };
    let doc = Document::load(pdf.to_bytes().unwrap()).unwrap();
    assert!(catalog(&doc).get("Metadata").is_none());
}

/// Asserts an outline item's `/Dest` resolves to a `/Type /Page` dict whose
/// `/MediaBox` matches `expected` — distinguishing "resolves to some page"
/// from "resolves to the right page".
fn assert_dest_page(doc: &Document, item: &Dict, expected: PageSize) {
    let dest = item.get_array("Dest").expect("/Dest present");
    assert_eq!(dest.len(), 5);
    assert_eq!(dest[1].as_name(), Some(&Name("XYZ".into())));
    assert!(dest[2].is_null() && dest[3].is_null() && dest[4].is_null());
    let target = doc.resolve(&dest[0]).expect("/Dest target resolves");
    let target = target.as_dict().expect("/Dest target is a dictionary");
    assert_eq!(target.get_name("Type"), Some(&Name("Page".into())));
    let media_box = target.get_array("MediaBox").expect("/MediaBox present");
    let (width, height) = expected.dimensions();
    assert_eq!(media_box[2].as_f64(), Some(f64::from(width)));
    assert_eq!(media_box[3].as_f64(), Some(f64::from(height)));
}

fn title_of(item: &Dict) -> String {
    decode_text_string(
        item.get("Title")
            .expect("/Title present")
            .as_str_bytes()
            .expect("/Title is a string"),
    )
}

/// Two top-level bookmarks, the first with one nested child — each pointing
/// at a page of a distinct size, so a `/Dest` that resolves to the wrong
/// page shows up as a mismatched `/MediaBox` rather than passing by luck.
#[test]
fn outline_tree_walks_prev_next_and_dest_pages() {
    let pages: Vec<Page> = [PageSize::A4, PageSize::Letter, PageSize::A5]
        .into_iter()
        .map(Page::new)
        .collect();
    let bookmarks = vec![
        Bookmark {
            title: "Chapter One".to_string(),
            page: 0,
            children: vec![Bookmark::new("Section 1.1", 1)],
        },
        Bookmark::new("Chapter Two", 2),
    ];
    let bytes = Pdf {
        pages,
        outline: Some(Outline { bookmarks }),
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();

    let catalog = catalog(&doc);
    let outlines_obj = catalog.get("Outlines").expect("/Outlines present").clone();
    let outlines = resolve_dict(&doc, &outlines_obj);
    assert_eq!(outlines.get_name("Type"), Some(&Name("Outlines".into())));
    assert_eq!(outlines.get_int("Count"), Some(3));

    let first_obj = outlines.get("First").expect("/First present").clone();
    let chapter_one = resolve_dict(&doc, &first_obj);
    assert_eq!(title_of(&chapter_one), "Chapter One");
    assert_eq!(chapter_one.get_ref("Parent"), outlines_obj.as_ref());
    assert!(chapter_one.get("Prev").is_none());
    assert_eq!(chapter_one.get_int("Count"), Some(1));

    let next_obj = chapter_one.get("Next").expect("/Next present").clone();
    let chapter_two = resolve_dict(&doc, &next_obj);
    assert_eq!(title_of(&chapter_two), "Chapter Two");
    assert_eq!(chapter_two.get_ref("Prev"), first_obj.as_ref());
    assert_eq!(chapter_two.get_ref("Parent"), outlines_obj.as_ref());
    assert!(chapter_two.get("Next").is_none());
    assert!(chapter_two.get("First").is_none());
    assert!(chapter_two.get("Count").is_none());
    assert_eq!(outlines.get("Last").unwrap().as_ref(), next_obj.as_ref());

    let child_obj = chapter_one.get("First").expect("/First present").clone();
    assert_eq!(
        chapter_one.get("Last").unwrap().as_ref(),
        child_obj.as_ref()
    );
    let section = resolve_dict(&doc, &child_obj);
    assert_eq!(title_of(&section), "Section 1.1");
    assert_eq!(section.get_ref("Parent"), first_obj.as_ref());
    assert!(section.get("Prev").is_none());
    assert!(section.get("Next").is_none());
    assert!(section.get("First").is_none());
    assert!(section.get("Count").is_none());

    assert_dest_page(&doc, &chapter_one, PageSize::A4);
    assert_dest_page(&doc, &section, PageSize::Letter);
    assert_dest_page(&doc, &chapter_two, PageSize::A5);
}

#[test]
fn outline_document_serializes_byte_identically() {
    fn build() -> Vec<u8> {
        Pdf {
            pages: vec![Page::new(PageSize::A4), Page::new(PageSize::A4)],
            outline: Some(Outline {
                bookmarks: vec![
                    Bookmark::new("One", 0),
                    Bookmark {
                        title: "Two".to_string(),
                        page: 1,
                        children: vec![Bookmark::new("Two.a", 0), Bookmark::new("Two.b", 1)],
                    },
                ],
            }),
            ..Pdf::default()
        }
        .to_bytes()
        .unwrap()
    }
    assert_eq!(build(), build());
}

#[test]
fn empty_outline_emits_no_outlines_entry() {
    let bytes = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        outline: Some(Outline::default()),
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    assert!(catalog(&doc).get("Outlines").is_none());
}

#[test]
fn bookmark_out_of_range_page_errors() {
    let err = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        outline: Some(Outline {
            bookmarks: vec![Bookmark::new("Nowhere", 9)],
        }),
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("out of range"), "{msg}");
    assert!(msg.contains("bookmark"), "{msg}");
}

#[test]
fn decode_sniffs_png_and_jpeg_by_content() {
    assert!(ImageData::decode(&[0x89, b'P', b'N', b'G']).is_err());
    assert!(ImageData::decode(b"plain text").is_err());
    let jpeg = tiny_jpeg(4, 4);
    assert_eq!(ImageData::decode(&jpeg).unwrap().width(), 4);
}

/// The `/Names /EmbeddedFiles /Names` array, as raw entries: alternating
/// name-tree keys and refs, straight off the catalog with no indirection
/// in between (the writer nests `/Names` and `/EmbeddedFiles` directly).
fn embedded_files_entries(doc: &Document) -> Vec<Object> {
    catalog(doc)
        .get_dict("Names")
        .expect("/Names present")
        .get_dict("EmbeddedFiles")
        .expect("/EmbeddedFiles present")
        .get_array("Names")
        .expect("/EmbeddedFiles /Names array present")
        .to_vec()
}

fn attachment(name: &str, data: &[u8], mime: Option<&str>) -> Attachment {
    Attachment {
        name: name.to_string(),
        data: data.to_vec(),
        mime: mime.map(str::to_string),
        modified: None,
        description: None,
    }
}

#[test]
fn no_attachments_emits_no_names_entry() {
    let bytes = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    assert!(catalog(&doc).get("Names").is_none());
}

/// Two attachments given in reverse lexical order must land sorted in the
/// emitted name tree, and each filespec's `/EF /F` stream must decode back
/// to the exact bytes given.
#[test]
fn attachments_sort_by_name_and_round_trip_bytes() {
    let pdf = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        attachments: vec![
            attachment("zeta.txt", b"zeta contents", Some("text/plain")),
            attachment("alpha.txt", b"alpha contents", Some("text/plain")),
        ],
        ..Pdf::default()
    };
    let doc = Document::load(pdf.to_bytes().unwrap()).unwrap();
    let entries = embedded_files_entries(&doc);
    assert_eq!(entries.len(), 4, "two attachments make four array slots");

    let key_at = |index: usize| {
        decode_text_string(
            entries[index]
                .as_str_bytes()
                .expect("name-tree key is a string"),
        )
    };
    assert_eq!(key_at(0), "alpha.txt", "sorted before zeta.txt");
    assert_eq!(key_at(2), "zeta.txt");

    let alpha_filespec = resolve_dict(&doc, &entries[1]);
    assert_eq!(
        alpha_filespec.get_name("Type"),
        Some(&Name("Filespec".into()))
    );
    assert_eq!(
        decode_text_string(alpha_filespec.get("F").unwrap().as_str_bytes().unwrap()),
        "alpha.txt"
    );
    assert_eq!(
        decode_text_string(alpha_filespec.get("UF").unwrap().as_str_bytes().unwrap()),
        "alpha.txt"
    );
    let ef = alpha_filespec.get_dict("EF").expect("/EF present");
    let stream = resolve_stream(&doc, ef.get("F").expect("/EF /F present"));
    assert_eq!(
        stream.dict.get_name("Type"),
        Some(&Name("EmbeddedFile".into()))
    );
    let decoded = doc.stream_data(&stream).expect("embedded stream decodes");
    assert_eq!(decoded, b"alpha contents");
}

/// A mime type containing `/` must come out of the writer's existing
/// `#xx` name-escaping as an escaped `/Subtype` — verified against the raw
/// emitted bytes, since a re-parsed `Name` stores its escapes already
/// resolved.
#[test]
fn attachment_subtype_escapes_mime_slash() {
    let pdf = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        attachments: vec![attachment("data.csv", b"a,b,c", Some("text/csv"))],
        ..Pdf::default()
    };
    let bytes = pdf.to_bytes().unwrap();
    assert!(
        contains(&bytes, b"/Subtype /text#2Fcsv"),
        "the mime's slash must be #-escaped in the emitted Subtype name"
    );
    let doc = Document::load(bytes).unwrap();
    let entries = embedded_files_entries(&doc);
    let filespec = resolve_dict(&doc, &entries[1]);
    let ef = filespec.get_dict("EF").expect("/EF present");
    let stream = resolve_stream(&doc, ef.get("F").expect("/EF /F present"));
    assert_eq!(
        stream.dict.get_name("Subtype"),
        Some(&Name("text/csv".into())),
        "decoded back to the unescaped mime"
    );
}

/// `/Params` always carries `/Size`; `/ModDate` appears only when
/// `modified` is supplied. A missing `mime` defaults to
/// `application/octet-stream`, and `/Desc` appears only when given.
#[test]
fn attachment_params_size_and_conditional_moddate() {
    let with_date = Attachment {
        description: Some("has a date".to_string()),
        modified: Some(Date {
            year: 2026,
            month: 8,
            day: 28,
            hour: 10,
            minute: 0,
            second: 0,
            utc_offset_minutes: 0,
        }),
        ..attachment("with-date.bin", &[1, 2, 3, 4, 5], None)
    };
    let without_date = attachment("no-date.bin", &[9, 9], None);
    let pdf = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        attachments: vec![with_date, without_date],
        ..Pdf::default()
    };
    let doc = Document::load(pdf.to_bytes().unwrap()).unwrap();
    let entries = embedded_files_entries(&doc);
    assert_eq!(entries.len(), 4);

    // "no-date.bin" sorts before "with-date.bin".
    let no_date_filespec = resolve_dict(&doc, &entries[1]);
    let no_date_ef = no_date_filespec.get_dict("EF").expect("/EF present");
    let no_date_stream = resolve_stream(&doc, no_date_ef.get("F").unwrap());
    assert_eq!(
        no_date_stream.dict.get_name("Subtype"),
        Some(&Name("application/octet-stream".into())),
        "no mime given defaults to application/octet-stream"
    );
    let no_date_params = no_date_stream
        .dict
        .get_dict("Params")
        .expect("/Params present");
    assert_eq!(no_date_params.get_int("Size"), Some(2));
    assert!(no_date_params.get("ModDate").is_none());
    assert!(no_date_filespec.get("Desc").is_none());

    let with_date_filespec = resolve_dict(&doc, &entries[3]);
    assert_eq!(
        decode_text_string(
            with_date_filespec
                .get("Desc")
                .unwrap()
                .as_str_bytes()
                .unwrap()
        ),
        "has a date"
    );
    let with_date_ef = with_date_filespec.get_dict("EF").expect("/EF present");
    let with_date_stream = resolve_stream(&doc, with_date_ef.get("F").unwrap());
    let with_date_params = with_date_stream
        .dict
        .get_dict("Params")
        .expect("/Params present");
    assert_eq!(with_date_params.get_int("Size"), Some(5));
    assert_eq!(
        with_date_params.get("ModDate").unwrap().as_str_bytes(),
        Some(b"D:20260828100000Z".as_slice())
    );
}

#[test]
fn duplicate_attachment_name_errors() {
    let pdf = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        attachments: vec![
            attachment("dup.txt", b"one", None),
            attachment("dup.txt", b"two", None),
        ],
        ..Pdf::default()
    };
    let err = pdf.to_bytes().unwrap_err();
    match err {
        Error::Other(msg) => assert!(msg.contains("dup.txt"), "{msg}"),
        other => panic!("expected Error::Other naming the duplicate, got {other:?}"),
    }
}

#[test]
fn attachments_document_serializes_byte_identically() {
    fn build() -> Vec<u8> {
        Pdf {
            pages: vec![Page::new(PageSize::A4)],
            attachments: vec![
                attachment("b.txt", b"B", Some("text/plain")),
                attachment("a.txt", b"A", Some("text/plain")),
            ],
            ..Pdf::default()
        }
        .to_bytes()
        .unwrap()
    }
    assert_eq!(build(), build());
}

fn four_pages() -> Vec<Page> {
    (0..4).map(|_| Page::new(PageSize::A4)).collect()
}

/// The design's own use case: roman-numeral front matter for pages 0–1,
/// then a decimal range from page 2 on with a chapter prefix and a
/// numbering offset — `/PageLabels /Nums` must carry both ranges, sorted
/// by `first_page`, each with the right `/S`, `/P` and `/St`.
#[test]
fn page_labels_resolve_roman_front_matter_and_offset_decimal() {
    let pdf = Pdf {
        pages: four_pages(),
        page_labels: vec![
            PageLabel {
                first_page: 0,
                style: Some(LabelStyle::RomanLower),
                prefix: None,
                start_at: 1,
            },
            PageLabel {
                first_page: 2,
                style: Some(LabelStyle::Decimal),
                prefix: Some("A-".to_string()),
                start_at: 5,
            },
        ],
        ..Pdf::default()
    };
    let doc = Document::load(pdf.to_bytes().unwrap()).unwrap();
    let page_labels = catalog(&doc)
        .get_dict("PageLabels")
        .expect("/PageLabels present")
        .clone();
    let nums = page_labels.get_array("Nums").expect("/Nums present");
    assert_eq!(nums.len(), 4, "two ranges make four Nums slots");

    let keys: Vec<i64> = (0..2)
        .map(|range_index| nums[range_index * 2].as_int().expect("key is an int"))
        .collect();
    assert_eq!(keys, vec![0, 2]);

    let front_matter = nums[1].as_dict().expect("range dict");
    assert_eq!(front_matter.get_name("S"), Some(&Name("r".into())));
    assert!(front_matter.get("P").is_none());
    assert!(front_matter.get("St").is_none());

    let body = nums[3].as_dict().expect("range dict");
    assert_eq!(body.get_name("S"), Some(&Name("D".into())));
    assert_eq!(
        decode_text_string(body.get("P").expect("/P present").as_str_bytes().unwrap()),
        "A-"
    );
    assert_eq!(body.get_int("St"), Some(5));
}

#[test]
fn page_labels_missing_zero_page_errors() {
    let pdf = Pdf {
        pages: four_pages(),
        page_labels: vec![PageLabel {
            first_page: 1,
            style: Some(LabelStyle::Decimal),
            prefix: None,
            start_at: 1,
        }],
        ..Pdf::default()
    };
    let err = pdf.to_bytes().unwrap_err();
    match err {
        Error::Other(msg) => assert!(msg.contains("page 0"), "{msg}"),
        other => panic!("expected Error::Other naming page 0, got {other:?}"),
    }
}

#[test]
fn page_labels_duplicate_first_page_errors() {
    let pdf = Pdf {
        pages: four_pages(),
        page_labels: vec![
            PageLabel {
                first_page: 0,
                style: None,
                prefix: None,
                start_at: 1,
            },
            PageLabel {
                first_page: 0,
                style: Some(LabelStyle::RomanUpper),
                prefix: None,
                start_at: 1,
            },
        ],
        ..Pdf::default()
    };
    let err = pdf.to_bytes().unwrap_err();
    match err {
        Error::Other(msg) => assert!(msg.contains("duplicate page label at page 0"), "{msg}"),
        other => panic!("expected Error::Other naming the duplicate page, got {other:?}"),
    }
}

#[test]
fn page_labels_zero_start_at_errors() {
    let pdf = Pdf {
        pages: four_pages(),
        page_labels: vec![PageLabel {
            first_page: 0,
            style: Some(LabelStyle::Decimal),
            prefix: None,
            start_at: 0,
        }],
        ..Pdf::default()
    };
    let err = pdf.to_bytes().unwrap_err();
    match err {
        Error::Other(msg) => assert!(msg.contains("page label at page 0 has start_at 0"), "{msg}"),
        other => panic!("expected Error::Other naming the zero start_at, got {other:?}"),
    }
}

#[test]
fn no_page_labels_emits_no_page_labels_entry() {
    let bytes = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    assert!(catalog(&doc).get("PageLabels").is_none());
}

/// Viewer preferences resolve structurally: `/PageLayout` and `/PageMode`
/// as catalog names, `/OpenAction` as an `/XYZ` destination landing on the
/// requested page.
#[test]
fn viewer_preferences_resolve_layout_mode_and_open_to() {
    let pdf = Pdf {
        pages: vec![Page::new(PageSize::A4), Page::new(PageSize::Letter)],
        viewer: Some(Viewer {
            layout: Some(PageLayout::TwoColumnLeft),
            mode: Some(PageMode::UseOutlines),
            open_to: Some(1),
        }),
        ..Pdf::default()
    };
    let doc = Document::load(pdf.to_bytes().unwrap()).unwrap();
    let catalog = catalog(&doc);
    assert_eq!(
        catalog.get_name("PageLayout"),
        Some(&Name("TwoColumnLeft".into()))
    );
    assert_eq!(
        catalog.get_name("PageMode"),
        Some(&Name("UseOutlines".into()))
    );
    let open_action = catalog
        .get_array("OpenAction")
        .expect("/OpenAction present");
    assert_eq!(open_action.len(), 5);
    assert_eq!(open_action[1].as_name(), Some(&Name("XYZ".into())));
    assert!(open_action[2].is_null() && open_action[3].is_null() && open_action[4].is_null());
    let target = doc
        .resolve(&open_action[0])
        .expect("/OpenAction target resolves");
    let target = target.as_dict().expect("target is a dictionary");
    assert_eq!(target.get_name("Type"), Some(&Name("Page".into())));
    let media_box = target.get_array("MediaBox").expect("/MediaBox present");
    assert_eq!(media_box[2].as_f64(), Some(612.0));
    assert_eq!(media_box[3].as_f64(), Some(792.0));
}

#[test]
fn no_viewer_emits_no_viewer_entries() {
    let bytes = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        ..Pdf::default()
    }
    .to_bytes()
    .unwrap();
    let doc = Document::load(bytes).unwrap();
    let catalog = catalog(&doc);
    assert!(catalog.get("PageLayout").is_none());
    assert!(catalog.get("PageMode").is_none());
    assert!(catalog.get("OpenAction").is_none());
}

#[test]
fn viewer_open_to_out_of_range_errors() {
    let pdf = Pdf {
        pages: vec![Page::new(PageSize::A4)],
        viewer: Some(Viewer {
            open_to: Some(5),
            ..Viewer::default()
        }),
        ..Pdf::default()
    };
    let err = pdf.to_bytes().unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("out of range"), "{msg}");
    assert!(msg.contains("open_to"), "{msg}");
}

#[test]
fn page_labels_and_viewer_document_serializes_byte_identically() {
    fn build() -> Vec<u8> {
        Pdf {
            pages: four_pages(),
            page_labels: vec![
                PageLabel {
                    first_page: 0,
                    style: Some(LabelStyle::RomanLower),
                    prefix: None,
                    start_at: 1,
                },
                PageLabel {
                    first_page: 2,
                    style: Some(LabelStyle::Decimal),
                    prefix: Some("A-".to_string()),
                    start_at: 5,
                },
            ],
            viewer: Some(Viewer {
                layout: Some(PageLayout::TwoColumnLeft),
                mode: Some(PageMode::UseOutlines),
                open_to: Some(1),
            }),
            ..Pdf::default()
        }
        .to_bytes()
        .unwrap()
    }
    assert_eq!(build(), build());
}
