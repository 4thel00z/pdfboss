//! End-to-end tests driving the `pdfboss` binary against the committed
//! fixture documents.

use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn pdfboss(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfboss"))
        .args(args)
        .output()
        .expect("failed to launch pdfboss binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn info_hello_reports_pages_and_size() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["info", file.to_str().unwrap()]);
    assert!(output.status.success(), "info failed: {output:?}");
    let text = stdout(&output);
    assert!(text.contains('1'), "no page count in: {text}");
    assert!(text.contains("612"), "no page width in: {text}");
    assert!(text.contains("encrypted: false"), "no flag in: {text}");
}

#[test]
fn info_three_pages_lists_each_page() {
    let file = fixture("three-pages.pdf");
    let output = pdfboss(&["info", file.to_str().unwrap()]);
    assert!(output.status.success(), "info failed: {output:?}");
    let text = stdout(&output);
    assert!(text.contains("pages:     3"), "wrong count in: {text}");
    assert!(text.contains("page 3:"), "missing page 3 in: {text}");
}

#[test]
fn obj_hello_prints_a_dict() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["obj", file.to_str().unwrap(), "1"]);
    assert!(output.status.success(), "obj failed: {output:?}");
    let text = stdout(&output);
    assert!(text.contains("<<"), "no dict open in: {text}");
    assert!(text.contains(">>"), "no dict close in: {text}");
}

#[test]
fn text_page_two_of_three() {
    let file = fixture("three-pages.pdf");
    let output = pdfboss(&["text", file.to_str().unwrap(), "--page", "2"]);
    assert!(output.status.success(), "text failed: {output:?}");
    let text = stdout(&output);
    assert!(text.contains("Page two"), "missing text in: {text}");
}

#[test]
fn text_all_pages_joined_by_form_feed() {
    let file = fixture("three-pages.pdf");
    let output = pdfboss(&["text", file.to_str().unwrap()]);
    assert!(output.status.success(), "text failed: {output:?}");
    let text = stdout(&output);
    assert!(text.contains("Page one"), "missing page one in: {text}");
    assert!(text.contains("Page three"), "missing page three in: {text}");
    assert_eq!(
        text.matches('\u{c}').count(),
        2,
        "expected two form feeds in: {text:?}"
    );
}

#[test]
fn render_smoke_writes_png() {
    let file = fixture("hello.pdf");
    let out = std::env::temp_dir().join(format!("pdfboss-cli-render-{}.png", std::process::id()));
    let output = pdfboss(&[
        "render",
        file.to_str().unwrap(),
        "--page",
        "1",
        "-o",
        out.to_str().unwrap(),
        "--scale",
        "0.5",
    ]);
    assert!(output.status.success(), "render failed: {output:?}");
    let bytes = std::fs::read(&out).expect("output PNG missing");
    let _ = std::fs::remove_file(&out);
    assert!(
        bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']),
        "output does not start with PNG magic"
    );
}

#[test]
fn obj_stream_prints_decoded_length_note() {
    // Walk the low object numbers until we hit the page's content stream;
    // its output must carry the decoded-byte note instead of raw data.
    let file = fixture("hello.pdf");
    let found = (1..=8u32).any(|num| {
        let output = pdfboss(&["obj", file.to_str().unwrap(), &num.to_string()]);
        output.status.success() && stdout(&output).contains("bytes decoded>")
    });
    assert!(found, "no stream object reported a decoded length");
}

#[test]
fn obj_missing_object_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["obj", file.to_str().unwrap(), "999"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}

#[test]
fn missing_file_exits_one_with_stderr() {
    let output = pdfboss(&["info", "definitely-not-here.pdf"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}

#[test]
fn out_of_range_page_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["text", file.to_str().unwrap(), "--page", "9"]);
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("out of range"), "unexpected stderr: {err}");
}
