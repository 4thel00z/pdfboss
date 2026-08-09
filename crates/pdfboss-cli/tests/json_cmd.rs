//! End-to-end tests for `pdfboss json`.

mod common;

use common::{assert_golden, fixture, pdfboss, stdout_str, strip_ansi};

#[test]
fn json_hello_matches_golden() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap()]);
    assert!(output.status.success(), "json failed: {output:?}");
    assert_golden("json-hello.txt", &strip_ansi(&stdout_str(&output)));
}

#[test]
fn json_dump_is_stable_across_runs() {
    let file = fixture("hello.pdf");
    let first = pdfboss(&["json", file.to_str().unwrap()]);
    let second = pdfboss(&["json", file.to_str().unwrap()]);
    assert!(first.status.success() && second.status.success());
    assert_eq!(
        stdout_str(&first),
        stdout_str(&second),
        "json dump must be deterministic"
    );
}

#[test]
fn json_raw_embeds_stream_data() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--raw"]);
    assert!(output.status.success(), "json --raw failed: {output:?}");
    assert!(stdout_str(&output).contains("\"data\""), "no data field");
}

#[test]
fn json_decode_embeds_decoded_stream_data() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--decode"]);
    assert!(output.status.success(), "json --decode failed: {output:?}");
    assert!(stdout_str(&output).contains("\"data\""), "no data field");
}

#[test]
fn json_no_logical_empties_pages() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--no-logical"]);
    assert!(output.status.success(), "json failed: {output:?}");
    assert!(
        stdout_str(&output).contains("\"pages\": []"),
        "pages not empty: {}",
        stdout_str(&output)
    );
}

#[test]
fn json_content_ops_lists_operator_spans() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--content-ops"]);
    assert!(output.status.success(), "json failed: {output:?}");
    let text = stdout_str(&output);
    assert!(text.contains("\"content_ops\""), "no content_ops: {text}");
    assert!(text.contains("\"_span_in_content\""), "no op spans: {text}");
}

#[test]
fn json_layout_adds_a_layout_array() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--layout"]);
    assert!(output.status.success(), "json --layout failed: {output:?}");
    let text = strip_ansi(&stdout_str(&output));
    let tree: serde_json::Value = serde_json::from_str(&text).expect("valid json output");
    let layout = tree["layout"].as_array().expect("layout is an array");
    assert_eq!(layout.len(), 1, "one page: {text}");
    assert!(text.contains("Hello, world!"), "no page text in: {text}");
}

#[test]
fn json_without_layout_omits_the_key() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap()]);
    assert!(output.status.success(), "json failed: {output:?}");
    let text = strip_ansi(&stdout_str(&output));
    let tree: serde_json::Value = serde_json::from_str(&text).expect("valid json output");
    assert!(tree.get("layout").is_none(), "unexpected layout: {text}");
}

#[test]
fn json_layout_with_pages_filter_ranks_only_the_requested_page() {
    let bytes = pdfboss_testkit::multi_page_doc(&["one", "two", "three"]);
    let path = std::env::temp_dir().join(format!(
        "pdfboss-json-layout-pages-{}.pdf",
        std::process::id()
    ));
    std::fs::write(&path, bytes).expect("write temp fixture");
    let output = pdfboss(&["json", path.to_str().unwrap(), "--pages", "2", "--layout"]);
    let _ = std::fs::remove_file(&path);
    assert!(output.status.success(), "json failed: {output:?}");
    let text = strip_ansi(&stdout_str(&output));
    let tree: serde_json::Value = serde_json::from_str(&text).expect("valid json output");
    let layout = tree["layout"].as_array().expect("layout is an array");
    assert_eq!(layout.len(), 1, "one requested page: {text}");
    assert!(text.contains("two"), "missing requested page text: {text}");
}

#[test]
fn json_pages_filter_keeps_only_selected_pages() {
    let bytes = pdfboss_testkit::multi_page_doc(&["one", "two", "three"]);
    let path = std::env::temp_dir().join(format!("pdfboss-json-pages-{}.pdf", std::process::id()));
    std::fs::write(&path, bytes).expect("write temp fixture");
    let output = pdfboss(&["json", path.to_str().unwrap(), "--pages", "2"]);
    let _ = std::fs::remove_file(&path);
    assert!(output.status.success(), "json failed: {output:?}");
    let text = stdout_str(&output);
    assert!(text.contains("\"index\": 1"), "page 2 missing: {text}");
    assert!(!text.contains("\"index\": 0"), "page 1 kept: {text}");
    assert!(!text.contains("\"index\": 2"), "page 3 kept: {text}");
}

/// Controller-required guard (plan 04 task 8, task 2's review): a
/// hybrid-reference file's xref section must reconcile to chain order
/// `[table, stream]` — classic section first, then the `/XRefStm` it points
/// at — with exactly one merged trailer, no matter how the underlying
/// document layer internally walks the chain.
#[test]
fn json_hybrid_doc_orders_xref_chain_and_merges_trailer() {
    let bytes = pdfboss_testkit::hybrid_doc();
    let path = std::env::temp_dir().join(format!("pdfboss-json-hybrid-{}.pdf", std::process::id()));
    std::fs::write(&path, bytes).expect("write temp fixture");
    let output = pdfboss(&["json", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(output.status.success(), "json failed: {output:?}");
    let text = strip_ansi(&stdout_str(&output));

    let tree: serde_json::Value = serde_json::from_str(&text).expect("valid json output");
    let kinds: Vec<&str> = tree["xref"]
        .as_array()
        .expect("xref is an array")
        .iter()
        .map(|entry| entry["kind"].as_str().expect("kind is a string"))
        .collect();
    assert_eq!(
        kinds,
        ["table", "stream"],
        "hybrid doc must reconcile to chain order [table, stream]: {text}"
    );
    assert!(
        tree["trailer"].is_object(),
        "expected a single merged trailer object, not an array: {text}"
    );

    assert_golden("json-hybrid.txt", &text);
}

#[test]
fn json_missing_file_exits_one() {
    let output = pdfboss(&["json", "definitely-not-here.pdf"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}

#[test]
fn json_raw_and_decode_conflict_is_a_usage_error() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["json", file.to_str().unwrap(), "--raw", "--decode"]);
    assert_eq!(output.status.code(), Some(2), "clap usage errors exit 2");
}
