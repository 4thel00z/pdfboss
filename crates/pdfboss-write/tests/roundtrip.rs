//! Oracle round-trip suite: documents built with `pdfboss-write` are read
//! back through `pdfboss-core`, extracted with `pdfboss-output` and
//! rasterized with `pdfboss-render`, so the writer is verified against the
//! toolkit's own readers rather than against expected byte dumps.

use pdfboss_core::object::decode_text_string;
use pdfboss_core::{Dict, Document, Name, Object, Rect, Stream};
use pdfboss_output::extract_text;
use pdfboss_render::{render_page_reporting, RenderOptions};
use pdfboss_write::{
    Bookmark, Color, Content, Date, Error, ImageData, Link, LinkAnnotation, LinkTarget, Metadata,
    Outline, Page, PageSize, Paragraph, Pdf, Standard14, WriteOptions, XrefStyle,
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
    let text = extract_text(&doc, &loaded).unwrap();
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
        assert!(extract_text(&doc, &loaded).unwrap().contains("Xref"));
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
    let text = extract_text(&doc, &loaded).unwrap();
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
