//! End-to-end tests for `pdfboss meta`, driving the binary and verifying
//! metadata updates, byte preservation, and error handling.

use std::path::PathBuf;
use std::process::Output;

mod common;

use pdfboss_core::Document;

fn tmp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn pdfboss(args: &[&str]) -> Output {
    common::pdfboss(args)
}

fn load(path: &PathBuf) -> Document {
    Document::open_with_password(path, "").expect("created PDF failed to load")
}

#[test]
fn meta_sets_title_and_preserves_base_bytes() {
    let input = common::fixture("hello.pdf");
    let out = tmp("meta-title.pdf");

    let output = pdfboss(&[
        "meta",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--set",
        "title=Renamed",
    ]);
    assert!(output.status.success(), "meta failed: {output:?}");

    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&out).unwrap();

    assert_eq!(
        input_bytes.len(),
        output_bytes[..input_bytes.len()].len(),
        "base file shorter than original"
    );
    assert_eq!(
        &input_bytes[..],
        &output_bytes[..input_bytes.len()],
        "base bytes were modified"
    );

    let doc = load(&out);
    assert_eq!(doc.metadata().title.as_deref(), Some("Renamed"));
}

#[test]
fn meta_on_xref_stream_fixture() {
    let input = common::fixture("xref-stream.pdf");
    let out = tmp("meta-xref-stream.pdf");

    let output = pdfboss(&[
        "meta",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--set",
        "title=XRefStream",
    ]);
    assert!(output.status.success(), "meta failed: {output:?}");

    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&out).unwrap();

    assert_eq!(
        &input_bytes[..],
        &output_bytes[..input_bytes.len()],
        "base bytes were modified on xref-stream fixture"
    );

    let doc = load(&out);
    assert_eq!(doc.metadata().title.as_deref(), Some("XRefStream"));
}

#[test]
fn meta_merges_repeated_sets() {
    let input = common::fixture("hello.pdf");
    let out = tmp("meta-merged.pdf");

    let output = pdfboss(&[
        "meta",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--set",
        "title=MyTitle",
        "--set",
        "author=MyAuthor",
    ]);
    assert!(output.status.success(), "meta failed: {output:?}");

    let doc = load(&out);
    assert_eq!(doc.metadata().title.as_deref(), Some("MyTitle"));
    assert_eq!(doc.metadata().author.as_deref(), Some("MyAuthor"));
}

#[test]
fn meta_unknown_key_exits_one_and_lists_keys() {
    let input = common::fixture("hello.pdf");
    let out = tmp("meta-bad-key.pdf");

    let output = pdfboss(&[
        "meta",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--set",
        "unknown=value",
    ]);
    assert_eq!(output.status.code(), Some(1), "expected exit code 1");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("valid keys are"),
        "stderr missing valid keys list: {stderr}"
    );
}

#[test]
fn meta_requires_set() {
    let input = common::fixture("hello.pdf");
    let out = tmp("meta-no-set.pdf");

    let output = pdfboss(&["meta", input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected usage error exit code 2"
    );
}

#[test]
fn meta_rewrite_flag_writes_a_fresh_file_with_the_set_fields() {
    let input = common::fixture("hello.pdf");
    let out = tmp("meta-rewrite.pdf");

    let output = pdfboss(&[
        "meta",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--set",
        "title=Rewritten",
        "--rewrite",
    ]);
    assert!(output.status.success(), "meta --rewrite failed: {output:?}");

    let input_bytes = std::fs::read(&input).unwrap();
    let output_bytes = std::fs::read(&out).unwrap();
    assert!(
        !output_bytes.starts_with(&input_bytes[..]),
        "--rewrite must not merely append an update onto the input"
    );

    let doc = load(&out);
    assert_eq!(doc.metadata().title.as_deref(), Some("Rewritten"));
}
