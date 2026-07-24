//! End-to-end tests for `pdfboss hex`.

mod common;

use common::{assert_golden, fixture, pdfboss, stdout_str, strip_ansi};

#[test]
fn hex_header_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "header"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-header.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_whole_file_annotated_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "--annotate"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    let text = strip_ansi(&stdout_str(&output));
    assert!(text.contains("── obj 3 0 ──"), "no object boundary: {text}");
    assert!(
        text.contains("── trailer ──"),
        "no trailer boundary: {text}"
    );
    assert_golden("hex-annotate.txt", &text);
}

#[test]
fn hex_obj_selector_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "obj:3"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-obj-3.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_obj_with_generation_matches_bare_obj() {
    let file = fixture("hello.pdf");
    let bare = pdfboss(&["hex", file.to_str().unwrap(), "obj:3"]);
    let with_gen = pdfboss(&["hex", file.to_str().unwrap(), "obj:3,0"]);
    assert!(bare.status.success() && with_gen.status.success());
    assert_eq!(stdout_str(&bare), stdout_str(&with_gen));
}

#[test]
fn hex_trailer_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "trailer"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-trailer.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_xref_selector_on_xref_stream_fixture_matches_golden() {
    let file = fixture("xref-stream.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "xref:0", "--annotate"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-xref-stream.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_range_with_width_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&[
        "hex",
        file.to_str().unwrap(),
        "range:0x0-0x10",
        "--width",
        "8",
    ]);
    assert!(output.status.success(), "hex failed: {output:?}");
    assert_golden("hex-range.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn hex_range_accepts_decimal_offsets() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "range:0-8"]);
    assert!(output.status.success(), "hex failed: {output:?}");
    // %PDF-1.7
    assert!(
        stdout_str(&output).contains("25 50 44 46"),
        "no %PDF bytes: {}",
        stdout_str(&output)
    );
}

#[test]
fn hex_bad_selector_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "bogus"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}

#[test]
fn hex_missing_object_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["hex", file.to_str().unwrap(), "obj:999"]);
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("999"), "unhelpful error: {err}");
}
