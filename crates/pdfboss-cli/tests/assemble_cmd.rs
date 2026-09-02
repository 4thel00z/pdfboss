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
            pdfboss_output::extract_text(&doc, &page, pdfboss_output::ReadingOrder::Content)
                .unwrap()
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

#[test]
fn rewrite_writes_a_fresh_file_preserving_pages() {
    let input = tmp("rewrite-in.pdf");
    std::fs::write(
        &input,
        pdfboss_testkit::multi_page_doc(&["one", "two", "three"]),
    )
    .unwrap();
    let out = tmp("rewrite-out.pdf");

    let output = pdfboss(&[
        "rewrite",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "rewrite failed: {output:?}");

    let doc = load(&out);
    assert_eq!(doc.page_count(), 3);
    let texts: Vec<String> = (0..3)
        .map(|i| {
            let page = doc.page(i).unwrap();
            pdfboss_output::extract_text(&doc, &page, pdfboss_output::ReadingOrder::Content)
                .unwrap()
        })
        .collect();
    assert!(texts[0].contains("one"), "page 0: {:?}", texts[0]);
    assert!(texts[1].contains("two"), "page 1: {:?}", texts[1]);
    assert!(texts[2].contains("three"), "page 2: {:?}", texts[2]);
}

#[test]
fn rewrite_names_the_path_of_an_encrypted_input() {
    let a = tmp("rewrite-encrypted.pdf");
    std::fs::write(&a, pdfboss_testkit::encrypted_rc4_doc("secret")).unwrap();
    let out = tmp("rewrite-encrypted-out.pdf");

    let output = pdfboss(&["rewrite", a.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("rewrite-encrypted.pdf"),
        "no input path in: {stderr}"
    );
    assert!(stderr.contains("encrypted"), "no cause in: {stderr}");
}

#[test]
fn overlay_appends_by_default_and_keeps_the_prefix() {
    let input = tmp("overlay-append-in.pdf");
    let base = pdfboss_testkit::multi_page_doc(&["one", "two", "three"]);
    std::fs::write(&input, &base).unwrap();
    let mark = tmp("overlay-append-mark.pdf");
    std::fs::write(&mark, pdfboss_testkit::multi_page_doc(&["mark"])).unwrap();
    let out = tmp("overlay-append-out.pdf");

    let output = pdfboss(&[
        "overlay",
        input.to_str().unwrap(),
        mark.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "overlay failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("wrote"), "no wrote message in: {stdout}");

    let out_bytes = std::fs::read(&out).unwrap();
    assert_eq!(
        &out_bytes[..base.len()],
        &base[..],
        "an append keeps the base bytes in place"
    );

    let doc = load(&out);
    assert_eq!(doc.page_count(), 3);
}

#[test]
fn overlay_under_draws_beneath_the_content() {
    let input = tmp("overlay-under-in.pdf");
    std::fs::write(
        &input,
        pdfboss_testkit::multi_page_doc(&["one", "two", "three"]),
    )
    .unwrap();
    let mark = tmp("overlay-under-mark.pdf");
    std::fs::write(&mark, pdfboss_testkit::multi_page_doc(&["mark"])).unwrap();
    let out = tmp("overlay-under-out.pdf");

    let output = pdfboss(&[
        "overlay",
        input.to_str().unwrap(),
        mark.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--under",
    ]);
    assert!(output.status.success(), "overlay failed: {output:?}");

    let doc = load(&out);
    let page = doc.page(0).unwrap();
    let content = page.content(&doc).unwrap();
    assert!(
        content.starts_with(b"q /PdfbossWatermark Do Q"),
        "content does not start with the overlay draw: {:?}",
        String::from_utf8_lossy(&content)
    );
}

#[test]
fn overlay_rewrite_flag_writes_a_full_rewrite() {
    let input = tmp("overlay-rewrite-in.pdf");
    let base = pdfboss_testkit::multi_page_doc(&["one", "two", "three"]);
    std::fs::write(&input, &base).unwrap();
    let mark = tmp("overlay-rewrite-mark.pdf");
    std::fs::write(&mark, pdfboss_testkit::multi_page_doc(&["mark"])).unwrap();
    let out = tmp("overlay-rewrite-out.pdf");

    let output = pdfboss(&[
        "overlay",
        input.to_str().unwrap(),
        mark.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--rewrite",
    ]);
    assert!(output.status.success(), "overlay failed: {output:?}");

    let out_bytes = std::fs::read(&out).unwrap();
    assert_ne!(
        &out_bytes[..base.len().min(out_bytes.len())],
        &base[..base.len().min(out_bytes.len())],
        "a rewrite does not keep the base bytes in place"
    );

    let doc = load(&out);
    assert_eq!(doc.page_count(), 3);
}

#[test]
fn overlay_names_the_path_of_an_encrypted_overlay() {
    let input = tmp("overlay-encrypted-in.pdf");
    std::fs::write(
        &input,
        pdfboss_testkit::multi_page_doc(&["one", "two", "three"]),
    )
    .unwrap();
    let mark = tmp("overlay-encrypted-mark.pdf");
    std::fs::write(&mark, pdfboss_testkit::encrypted_rc4_doc("secret")).unwrap();
    let out = tmp("overlay-encrypted-out.pdf");

    let output = pdfboss(&[
        "overlay",
        input.to_str().unwrap(),
        mark.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("overlay-encrypted-mark.pdf"),
        "no overlay path in: {stderr}"
    );
    assert!(stderr.contains("encrypted"), "no cause in: {stderr}");
}

#[test]
fn encrypt_then_text_with_password_reads_it() {
    let input = tmp("encrypt-read-in.pdf");
    std::fs::write(&input, pdfboss_testkit::multi_page_doc(&["one", "two"])).unwrap();
    let out = tmp("encrypt-read-out.pdf");

    let output = pdfboss(&[
        "encrypt",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--user-password",
        "secret",
    ]);
    assert!(output.status.success(), "encrypt failed: {output:?}");

    let doc =
        Document::open_with_password(&out, "secret").expect("output opens with the user password");
    assert!(doc.is_encrypted(), "encrypted output reports unencrypted");

    let text_output = pdfboss(&["text", out.to_str().unwrap(), "--password", "secret"]);
    assert!(text_output.status.success(), "text failed: {text_output:?}");
    let stdout = String::from_utf8_lossy(&text_output.stdout).into_owned();
    assert!(stdout.contains("one"), "page 1 missing: {stdout:?}");
    assert!(stdout.contains("two"), "page 2 missing: {stdout:?}");
}

#[test]
fn encrypt_re_encrypts_an_already_encrypted_input_under_new_passwords() {
    let input = tmp("encrypt-re-encrypt-in.pdf");
    std::fs::write(&input, pdfboss_testkit::multi_page_doc(&["once"])).unwrap();
    let first = tmp("encrypt-re-encrypt-first.pdf");
    let first_output = pdfboss(&[
        "encrypt",
        input.to_str().unwrap(),
        "-o",
        first.to_str().unwrap(),
        "--user-password",
        "old",
    ]);
    assert!(
        first_output.status.success(),
        "first encrypt failed: {first_output:?}"
    );

    let second = tmp("encrypt-re-encrypt-second.pdf");
    let second_output = pdfboss(&[
        "encrypt",
        first.to_str().unwrap(),
        "-o",
        second.to_str().unwrap(),
        "--password",
        "old",
        "--user-password",
        "new",
    ]);
    assert!(
        second_output.status.success(),
        "re-encrypt failed: {second_output:?}"
    );

    // The old password no longer opens the re-encrypted output.
    assert!(Document::open_with_password(&second, "old").is_err());

    let text_output = pdfboss(&["text", second.to_str().unwrap(), "--password", "new"]);
    assert!(text_output.status.success(), "text failed: {text_output:?}");
    assert!(String::from_utf8_lossy(&text_output.stdout).contains("once"));
}

#[test]
fn decrypt_of_encrypted_output_opens_with_plain_text() {
    let input = tmp("decrypt-read-in.pdf");
    std::fs::write(&input, pdfboss_testkit::multi_page_doc(&["alpha", "beta"])).unwrap();
    let encrypted = tmp("decrypt-read-encrypted.pdf");
    let encrypt_output = pdfboss(&[
        "encrypt",
        input.to_str().unwrap(),
        "-o",
        encrypted.to_str().unwrap(),
        "--user-password",
        "opensesame",
    ]);
    assert!(
        encrypt_output.status.success(),
        "encrypt failed: {encrypt_output:?}"
    );

    let out = tmp("decrypt-read-out.pdf");
    let decrypt_output = pdfboss(&[
        "decrypt",
        encrypted.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--password",
        "opensesame",
    ]);
    assert!(
        decrypt_output.status.success(),
        "decrypt failed: {decrypt_output:?}"
    );

    let doc = load(&out);
    assert!(!doc.is_encrypted(), "decrypted output still encrypted");

    let text_output = pdfboss(&["text", out.to_str().unwrap()]);
    assert!(text_output.status.success(), "text failed: {text_output:?}");
    let stdout = String::from_utf8_lossy(&text_output.stdout).into_owned();
    assert!(stdout.contains("alpha"), "page 1 missing: {stdout:?}");
    assert!(stdout.contains("beta"), "page 2 missing: {stdout:?}");
}

#[test]
fn decrypt_wrong_or_missing_password_exits_nonzero_naming_the_file() {
    let input = tmp("decrypt-wrong-in.pdf");
    std::fs::write(&input, pdfboss_testkit::multi_page_doc(&["one"])).unwrap();
    let encrypted = tmp("decrypt-wrong-encrypted.pdf");
    let encrypt_output = pdfboss(&[
        "encrypt",
        input.to_str().unwrap(),
        "-o",
        encrypted.to_str().unwrap(),
        "--user-password",
        "correct",
    ]);
    assert!(
        encrypt_output.status.success(),
        "encrypt failed: {encrypt_output:?}"
    );

    let out = tmp("decrypt-wrong-out.pdf");
    let wrong_password = pdfboss(&[
        "decrypt",
        encrypted.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--password",
        "wrong",
    ]);
    assert_ne!(wrong_password.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&wrong_password.stderr).into_owned();
    assert!(
        stderr.contains("decrypt-wrong-encrypted.pdf"),
        "no input path in: {stderr}"
    );
    assert!(!out.exists(), "no output should be written on a bad open");

    let missing_password = pdfboss(&[
        "decrypt",
        encrypted.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_ne!(missing_password.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&missing_password.stderr).into_owned();
    assert!(
        stderr.contains("decrypt-wrong-encrypted.pdf"),
        "no input path in: {stderr}"
    );
}

#[test]
fn encrypt_bad_allow_value_exits_2() {
    let input = tmp("encrypt-bad-allow-in.pdf");
    std::fs::write(&input, pdfboss_testkit::multi_page_doc(&["one"])).unwrap();
    let out = tmp("encrypt-bad-allow-out.pdf");

    let output = pdfboss(&[
        "encrypt",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--user-password",
        "secret",
        "--allow",
        "print,bogus",
    ]);
    assert_eq!(output.status.code(), Some(2), "output: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("bogus"), "no offending value in: {stderr}");
    for name in [
        "print",
        "modify",
        "copy",
        "annotate",
        "fill-forms",
        "accessibility",
        "assemble",
        "print-hires",
    ] {
        assert!(stderr.contains(name), "{name} missing from list: {stderr}");
    }
}

#[test]
fn encrypt_requires_at_least_one_non_empty_password() {
    let input = tmp("encrypt-no-password-in.pdf");
    std::fs::write(&input, pdfboss_testkit::multi_page_doc(&["one"])).unwrap();
    let out = tmp("encrypt-no-password-out.pdf");

    let output = pdfboss(&[
        "encrypt",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1), "output: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("--user-password") || stderr.contains("--owner-password"),
        "no flag named in: {stderr}"
    );
}

#[test]
fn encrypt_with_only_owner_password_opens_with_owner_and_empty_user_password() {
    let input = tmp("encrypt-owner-only-in.pdf");
    std::fs::write(&input, pdfboss_testkit::multi_page_doc(&["secretpage"])).unwrap();
    let out = tmp("encrypt-owner-only-out.pdf");

    let output = pdfboss(&[
        "encrypt",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--owner-password",
        "ownersecret",
    ]);
    assert!(output.status.success(), "encrypt failed: {output:?}");

    // Opens with the owner password.
    let owner_text = pdfboss(&["text", out.to_str().unwrap(), "--password", "ownersecret"]);
    assert!(
        owner_text.status.success(),
        "owner-password open failed: {owner_text:?}"
    );
    assert!(String::from_utf8_lossy(&owner_text.stdout).contains("secretpage"));

    // ISO empty-user-password case: also opens with no password supplied.
    let empty_user_text = pdfboss(&["text", out.to_str().unwrap()]);
    assert!(
        empty_user_text.status.success(),
        "empty-user-password open failed: {empty_user_text:?}"
    );
    assert!(String::from_utf8_lossy(&empty_user_text.stdout).contains("secretpage"));
}
