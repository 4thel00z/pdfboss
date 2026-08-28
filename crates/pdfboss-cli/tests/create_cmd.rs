//! End-to-end tests for `pdfboss create`, driving the binary and loading
//! the results back through `pdfboss-core`.

use std::path::PathBuf;
use std::process::{Command, Output};

use pdfboss_core::Document;

fn tmp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn pdfboss(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfboss"))
        .args(args)
        .output()
        .expect("failed to launch pdfboss binary")
}

fn load(path: &PathBuf) -> Document {
    Document::open_with_password(path, "").expect("created PDF failed to load")
}

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&vec![10u8; (width * height * 3) as usize])
        .unwrap();
    writer.finish().unwrap();
    bytes
}

/// SOI, then a baseline SOF0 declaring 8-bit 3 × 2 grayscale — enough for
/// the passthrough import to sniff dimensions from.
fn jpeg_bytes() -> Vec<u8> {
    let (soi, sof0) = ([0xFF, 0xD8], [0xFF, 0xC0]);
    let (len, precision, height, width, components) = (11u16, 8u8, 2u16, 3u16, 1u8);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&soi);
    bytes.extend_from_slice(&sof0);
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.push(precision);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.push(components);
    bytes
}

#[test]
fn blank_creates_loadable_pages() {
    let out = tmp("blank.pdf");
    let output = pdfboss(&[
        "create",
        "blank",
        "-o",
        out.to_str().unwrap(),
        "--pages",
        "3",
    ]);
    assert!(output.status.success(), "create blank failed: {output:?}");
    let doc = load(&out);
    assert_eq!(doc.page_count(), 3);
    let (w, h) = doc.page(0).unwrap().size();
    assert!((w - 595.28).abs() < 0.01, "unexpected width {w}");
    assert!((h - 841.89).abs() < 0.01, "unexpected height {h}");
}

#[test]
fn blank_landscape_letter_swaps_dimensions() {
    let out = tmp("blank-landscape.pdf");
    let output = pdfboss(&[
        "create",
        "blank",
        "-o",
        out.to_str().unwrap(),
        "--size",
        "letter",
        "--landscape",
    ]);
    assert!(output.status.success(), "create blank failed: {output:?}");
    let doc = load(&out);
    assert_eq!(doc.page_count(), 1);
    let (w, h) = doc.page(0).unwrap().size();
    assert!((w - 792.0).abs() < 0.01, "unexpected width {w}");
    assert!((h - 612.0).abs() < 0.01, "unexpected height {h}");
}

#[test]
fn text_round_trips_through_extraction() {
    let input = tmp("text-input.txt");
    std::fs::write(&input, "Hello from pdfboss create\nSecond line").unwrap();
    let out = tmp("text.pdf");
    let output = pdfboss(&[
        "create",
        "text",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "create text failed: {output:?}");
    let extracted = pdfboss(&["text", out.to_str().unwrap()]);
    assert!(extracted.status.success(), "text failed: {extracted:?}");
    let text = String::from_utf8_lossy(&extracted.stdout).into_owned();
    assert!(
        text.contains("Hello from pdfboss create"),
        "line lost in: {text}"
    );
    assert!(text.contains("Second line"), "line lost in: {text}");
}

#[test]
fn text_overflow_starts_new_pages() {
    let input = tmp("text-long.txt");
    let lines: Vec<String> = (1..=200).map(|i| format!("line {i}")).collect();
    std::fs::write(&input, lines.join("\n")).unwrap();
    let out = tmp("text-long.pdf");
    let output = pdfboss(&[
        "create",
        "text",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "create text failed: {output:?}");
    let doc = load(&out);
    assert!(doc.page_count() > 1, "200 lines fit one page?");
}

#[test]
fn text_unencodable_char_fails_with_line_number() {
    let input = tmp("text-bad.txt");
    std::fs::write(&input, "fine\nbad \u{2318} here").unwrap();
    let out = tmp("text-bad.pdf");
    let output = pdfboss(&[
        "create",
        "text",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("line 2"), "no line number in: {stderr}");
    assert!(stderr.contains('\u{2318}'), "no character in: {stderr}");
}

#[test]
fn images_make_one_page_per_image_sized_to_pixels() {
    let first = tmp("img-a.png");
    let second = tmp("img-b.png");
    std::fs::write(&first, png_bytes(3, 2)).unwrap();
    std::fs::write(&second, png_bytes(5, 4)).unwrap();
    let out = tmp("images.pdf");
    let output = pdfboss(&[
        "create",
        "images",
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "create images failed: {output:?}");
    let doc = load(&out);
    assert_eq!(doc.page_count(), 2);
    assert_eq!(doc.page(0).unwrap().size(), (3.0, 2.0));
    assert_eq!(doc.page(1).unwrap().size(), (5.0, 4.0));
    let page = doc.page(0).unwrap();
    let xobjects = page
        .resources
        .get("XObject")
        .and_then(|o| o.as_dict())
        .expect("no XObject resources");
    let image_ref = xobjects
        .get("Im1")
        .and_then(|o| o.as_ref())
        .expect("no Im1 reference");
    let object = doc.get(image_ref).unwrap();
    let stream = object.as_stream().expect("image is not a stream");
    assert_eq!(
        stream.dict.get_name("Subtype").map(|n| n.0.as_str()),
        Some("Image")
    );
    assert_eq!(stream.dict.get_int("Width"), Some(3));
    assert_eq!(stream.dict.get_int("Height"), Some(2));
}

#[test]
fn images_detect_kind_by_magic_not_extension() {
    let input = tmp("actually-jpeg.png");
    std::fs::write(&input, jpeg_bytes()).unwrap();
    let out = tmp("images-magic.pdf");
    let output = pdfboss(&[
        "create",
        "images",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "create images failed: {output:?}");
    let doc = load(&out);
    assert_eq!(doc.page_count(), 1);
    assert_eq!(doc.page(0).unwrap().size(), (3.0, 2.0));
}

#[test]
fn images_with_size_scale_into_the_page() {
    let input = tmp("img-a4.png");
    std::fs::write(&input, png_bytes(3, 2)).unwrap();
    let out = tmp("images-a4.pdf");
    let output = pdfboss(&[
        "create",
        "images",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--size",
        "a4",
    ]);
    assert!(output.status.success(), "create images failed: {output:?}");
    let doc = load(&out);
    let (w, h) = doc.page(0).unwrap().size();
    assert!((w - 595.28).abs() < 0.01, "unexpected width {w}");
    assert!((h - 841.89).abs() < 0.01, "unexpected height {h}");
}

const Q3_MANIFEST: &str = r#"
[meta]
title  = "Q3 Report"
author = "Mo"

[[page]]
size = "a4"

  [[page.text]]
  value = "Q3 Report"
  at    = [72, 770]
  font  = "Helvetica-Bold"
  size  = 28

  [[page.paragraph]]
  value   = "Body copy for the quarter."
  rect    = [72, 380, 523, 720]
  size    = 11
  leading = 15
  align   = "left"

  [[page.image]]
  path  = "chart.png"
  at    = [72, 96]
  width = 200

  [[page.link]]
  rect = [72, 88, 523, 380]
  url  = "https://example.com/q3"

[[page]]
"#;

/// The design's own exit test for the TOML manifest: a q3-style manifest
/// with a tiny PNG fixture on disk and a second bare `[[page]]`, loaded back
/// through `pdfboss-core` — two pages, the text reaches the content stream,
/// the metadata reaches `/Info`, and the link's `/URI` resolves structurally
/// (mirroring the markdown crate's own roundtrip idiom: `/Annots` -> `Annot`
/// dict -> `/A` -> `/URI`, since the annotation dict is packed into a
/// compressed object stream and never appears literally in the file).
#[test]
fn manifest_maps_toml_through_the_compose_layer() {
    let dir = tmp("manifest");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("chart.png"), png_bytes(3, 2)).unwrap();
    let manifest_path = dir.join("q3.toml");
    std::fs::write(&manifest_path, Q3_MANIFEST).unwrap();
    let out = dir.join("q3.pdf");

    let output = pdfboss(&[
        "create",
        "manifest",
        manifest_path.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "create manifest failed: {output:?}"
    );

    let doc = load(&out);
    assert_eq!(doc.page_count(), 2);
    assert_eq!(doc.metadata().title.as_deref(), Some("Q3 Report"));
    assert_eq!(doc.metadata().author.as_deref(), Some("Mo"));

    let page = doc.page(0).unwrap();
    let text = pdfboss_output::extract_text(&doc, &page).unwrap();
    assert!(text.contains("Q3 Report"), "text was: {text}");

    let annots = page.dict().get_array("Annots").unwrap_or(&[]);
    let uris: Vec<Vec<u8>> = annots
        .iter()
        .filter_map(|annot| doc.resolve(annot).ok())
        .filter_map(|annot| annot.as_dict().cloned())
        .filter_map(|annot| annot.get_dict("A").cloned())
        .filter_map(|action| action.get("URI").cloned())
        .filter_map(|uri| uri.as_str_bytes().map(<[u8]>::to_vec))
        .collect();
    assert_eq!(uris, vec![b"https://example.com/q3".to_vec()]);
}

#[test]
fn images_reject_files_without_image_magic() {
    let input = tmp("not-an-image.txt");
    std::fs::write(&input, "just words").unwrap();
    let out = tmp("images-bad.pdf");
    let output = pdfboss(&[
        "create",
        "images",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("not a png or jpeg"),
        "unexpected message: {stderr}"
    );
    assert!(
        stderr.contains("not-an-image.txt"),
        "no file name in: {stderr}"
    );
}
