//! End-to-end tests for `pdfboss q`.

mod common;

use common::{assert_golden, fixture, pdfboss, stdout_str, strip_ansi};

#[test]
fn q_object_three_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), r#".objects["3 0"]"#]);
    assert!(output.status.success(), "q failed: {output:?}");
    assert_golden("q-object-3-0.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn q_select_over_kind_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&[
        "q",
        file.to_str().unwrap(),
        r#"[.objects[] | select(._kind == "object") | ._ref[0]] | sort"#,
    ]);
    assert!(output.status.success(), "q failed: {output:?}");
    assert_golden("q-select-kind.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn q_hex_dumps_span_ranges_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), ".header", "--hex"]);
    assert!(output.status.success(), "q failed: {output:?}");
    let text = strip_ansi(&stdout_str(&output));
    assert!(text.starts_with("── 0x0..0x"), "no range heading: {text}");
    assert_golden("q-hex-header.txt", &text);
}

#[test]
fn q_raw_strings_print_unquoted() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), ".header.version", "-r"]);
    assert!(output.status.success(), "q failed: {output:?}");
    assert_eq!(stdout_str(&output), "1.7\n");
}

#[test]
fn q_objstm_members_expose_their_container() {
    let file = fixture("xref-stream.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), r#".objects["1 0"]._objstm._r"#]);
    assert!(output.status.success(), "q failed: {output:?}");
    assert_eq!(strip_ansi(&stdout_str(&output)), "[\n  6,\n  0\n]\n");
}

#[test]
fn q_compile_error_exits_two_with_position() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), ".foo|"]);
    assert_eq!(output.status.code(), Some(2), "program errors exit 2");
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("jq"), "no jq marker: {err}");
    assert!(err.contains("byte"), "no position: {err}");
}

#[test]
fn q_runtime_error_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["q", file.to_str().unwrap(), r#"error("boom")"#]);
    assert_eq!(output.status.code(), Some(1), "runtime errors exit 1");
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("boom"), "message lost: {err}");
}

#[test]
fn q_missing_file_exits_one() {
    let output = pdfboss(&["q", "definitely-not-here.pdf", "."]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}
