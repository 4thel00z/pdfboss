//! End-to-end tests for `pdfboss merge`, driving the binary and loading the
//! results back through `pdfboss-core`.

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
