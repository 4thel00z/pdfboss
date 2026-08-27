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
    assert!(stderr.contains("magic"), "unexpected message: {stderr}");
    assert!(
        stderr.contains("not-an-image.txt"),
        "no file name in: {stderr}"
    );
}
