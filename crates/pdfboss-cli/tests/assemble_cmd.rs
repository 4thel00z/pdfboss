//! End-to-end tests for `pdfboss merge` and `pdfboss split`, driving the
//! binary and loading the results back through `pdfboss-core`.

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
    Document::open_with_password(path, "").expect("merged PDF failed to load")
}

#[test]
fn merge_combines_a_whole_file_and_a_selected_page() {
    let a = tmp("merge-a.pdf");
    let b = tmp("merge-b.pdf");
    std::fs::write(&a, pdfboss_testkit::multi_page_doc(&["a1", "a2"])).unwrap();
    std::fs::write(&b, pdfboss_testkit::multi_page_doc(&["b1", "b2", "b3"])).unwrap();
    let out = tmp("merge-out.pdf");

    let spec_b = format!("{}:2", b.to_str().unwrap());
    let output = pdfboss(&[
        "merge",
        a.to_str().unwrap(),
        &spec_b,
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "merge failed: {output:?}");

    let doc = load(&out);
    assert_eq!(doc.page_count(), 3);
    let texts: Vec<String> = (0..3)
        .map(|i| {
            let page = doc.page(i).unwrap();
            pdfboss_output::extract_text(&doc, &page).unwrap()
        })
        .collect();
    assert!(texts[0].contains("a1"), "page 0: {:?}", texts[0]);
    assert!(texts[1].contains("a2"), "page 1: {:?}", texts[1]);
    assert!(texts[2].contains("b2"), "page 2: {:?}", texts[2]);
}

#[test]
fn merge_reports_a_bad_range_naming_the_page_and_count() {
    let a = tmp("merge-bad-range.pdf");
    std::fs::write(&a, pdfboss_testkit::multi_page_doc(&["a1", "a2"])).unwrap();
    let out = tmp("merge-bad-range-out.pdf");

    let spec = format!("{}:9", a.to_str().unwrap());
    let output = pdfboss(&["merge", &spec, "-o", out.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("page 9"), "no page number in: {stderr}");
    assert!(stderr.contains("2 pages"), "no page count in: {stderr}");
}

#[test]
fn merge_names_the_path_of_an_encrypted_input() {
    let a = tmp("merge-encrypted.pdf");
    std::fs::write(&a, pdfboss_testkit::encrypted_rc4_doc("secret")).unwrap();
    let out = tmp("merge-encrypted-out.pdf");

    let output = pdfboss(&["merge", a.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("merge-encrypted.pdf"),
        "no input path in: {stderr}"
    );
    assert!(stderr.contains("encrypted"), "no cause in: {stderr}");
}

fn twenty_five_page_doc() -> Vec<u8> {
    let labels: Vec<String> = (1..=25).map(|i| i.to_string()).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    pdfboss_testkit::multi_page_doc(&refs)
}

#[test]
fn split_writes_one_part_per_chunk_named_by_pattern() {
    let dir = tmp("split-parts");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("big.pdf");
    std::fs::write(&input, twenty_five_page_doc()).unwrap();
    let pattern = dir.join("part-%d.pdf");

    let output = pdfboss(&[
        "split",
        input.to_str().unwrap(),
        "-o",
        pattern.to_str().unwrap(),
        "--every",
        "10",
    ]);
    assert!(output.status.success(), "split failed: {output:?}");

    let expected_counts = [10, 10, 5];
    for (i, expected) in expected_counts.iter().enumerate() {
        let part = dir.join(format!("part-{}.pdf", i + 1));
        let doc = load(&part);
        assert_eq!(doc.page_count(), *expected, "part {}", i + 1);
    }
    assert!(
        !dir.join("part-4.pdf").exists(),
        "no fourth part should be written"
    );
}

#[test]
fn split_rejects_a_missing_percent_d_before_opening_the_input() {
    let missing_input = tmp("split-does-not-exist.pdf");

    let output = pdfboss(&[
        "split",
        missing_input.to_str().unwrap(),
        "-o",
        "part.pdf",
        "--every",
        "10",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("%d"), "no %d complaint in: {stderr}");
    assert!(
        !stderr.contains("does-not-exist"),
        "input was opened despite the bad pattern: {stderr}"
    );
}

#[test]
fn split_rejects_every_zero_at_the_clap_level() {
    let output = pdfboss(&["split", "in.pdf", "-o", "part-%d.pdf", "--every", "0"]);
    assert_eq!(output.status.code(), Some(2), "output: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("--every"),
        "no --every mention in: {stderr}"
    );
    assert!(stderr.contains('0'), "no offending value in: {stderr}");
}

#[test]
fn rotate_appends_by_default_and_keeps_the_prefix() {
    let input = tmp("rotate-append-in.pdf");
    let base = pdfboss_testkit::multi_page_doc(&["one", "two", "three"]);
    std::fs::write(&input, &base).unwrap();
    let out = tmp("rotate-append-out.pdf");

    let output = pdfboss(&[
        "rotate",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--by",
        "90",
    ]);
    assert!(output.status.success(), "rotate failed: {output:?}");

    let out_bytes = std::fs::read(&out).unwrap();
    assert_eq!(
        &out_bytes[..base.len()],
        &base[..],
        "an append keeps the base bytes in place"
    );

    let doc = load(&out);
    for index in 0..3 {
        let page = doc.page(index).unwrap();
        assert_eq!(page.rotate, 90, "page {index}");
    }
}

#[test]
fn rotate_pages_flag_selects_a_range() {
    let input = tmp("rotate-range-in.pdf");
    std::fs::write(
        &input,
        pdfboss_testkit::multi_page_doc(&["one", "two", "three"]),
    )
    .unwrap();
    let out = tmp("rotate-range-out.pdf");

    let output = pdfboss(&[
        "rotate",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--pages",
        "2-3",
        "--by",
        "180",
    ]);
    assert!(output.status.success(), "rotate failed: {output:?}");

    let doc = load(&out);
    for (index, expected) in [0, 180, 180].iter().enumerate() {
        let page = doc.page(index).unwrap();
        assert_eq!(page.rotate, *expected, "page {index}");
    }
}

#[test]
fn rotate_rewrite_flag_writes_a_full_rewrite() {
    let input = tmp("rotate-rewrite-in.pdf");
    std::fs::write(
        &input,
        pdfboss_testkit::multi_page_doc(&["one", "two", "three"]),
    )
    .unwrap();
    let out = tmp("rotate-rewrite-out.pdf");

    let output = pdfboss(&[
        "rotate",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--by",
        "270",
        "--rewrite",
    ]);
    assert!(output.status.success(), "rotate failed: {output:?}");

    let doc = load(&out);
    for index in 0..3 {
        let page = doc.page(index).unwrap();
        assert_eq!(page.rotate, 270, "page {index}");
    }
}
