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

// `setsid(2)`: detaches the calling process into a new session with no
// controlling terminal. Declared by hand (rather than pulling in the
// `libc` crate) since it is the one FFI call this test file needs.
#[cfg(unix)]
extern "C" {
    fn setsid() -> i32;
}

/// Runs `pdfboss` with no controlling terminal at all, regardless of
/// whether this test process itself has one.
///
/// `Command::output` pipes stdout/stderr but leaves stdin inherited; on
/// Unix, crossterm's `enable_raw_mode` falls back to opening `/dev/tty` (the
/// *session's* controlling terminal) whenever stdin isn't itself a tty, so
/// merely piping stdio is not enough to force the no-tty path -- run under
/// an interactive `cargo test`, the child would still find the real
/// terminal via `/dev/tty` and the tui event loop would block on real
/// keyboard input instead of exiting. `setsid` in a `pre_exec` hook (run in
/// the forked child, before `exec`) detaches it from any controlling
/// terminal, so `/dev/tty` reliably fails with `ENXIO`/`ENOTTY` here no
/// matter where the test suite runs.
#[cfg(unix)]
fn pdfboss_without_a_terminal(args: &[&str]) -> Output {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pdfboss"));
    cmd.args(args);
    // SAFETY: `setsid` takes no arguments and is async-signal-safe, so it
    // is sound to call between `fork` and `exec`.
    unsafe {
        cmd.pre_exec(|| {
            if setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.output().expect("failed to launch pdfboss binary")
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
fn md_hello_prints_a_plain_paragraph() {
    // hello.pdf's single 12pt line has no heading ladder: plain paragraph.
    let file = fixture("hello.pdf");
    let output = pdfboss(&["md", file.to_str().unwrap()]);
    assert!(output.status.success(), "md failed: {output:?}");
    assert_eq!(stdout(&output).trim_end(), "Hello, world!");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn md_page_two_of_three() {
    let file = fixture("three-pages.pdf");
    let output = pdfboss(&["md", file.to_str().unwrap(), "--page", "2"]);
    assert!(output.status.success(), "md failed: {output:?}");
    let text = stdout(&output);
    assert!(text.contains("Page two"), "missing text in: {text}");
}

#[test]
fn md_missing_file_exits_one() {
    let output = pdfboss(&["md", "definitely-not-here.pdf"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty(), "expected an error message");
}

#[test]
fn md_out_of_range_page_exits_one() {
    let file = fixture("hello.pdf");
    let output = pdfboss(&["md", file.to_str().unwrap(), "--page", "9"]);
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(err.contains("out of range"), "unexpected stderr: {err}");
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

/// Renders page 1 of `hello.pdf` at half scale into a temp file with the
/// given extension; returns the process output and the bytes written (if
/// any), removing the file again.
fn render_hello_to(extension: &str) -> (Output, Option<Vec<u8>>) {
    let file = fixture("hello.pdf");
    let out = std::env::temp_dir().join(format!(
        "pdfboss-cli-render-{}.{extension}",
        std::process::id()
    ));
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
    let bytes = std::fs::read(&out).ok();
    let _ = std::fs::remove_file(&out);
    (output, bytes)
}

#[test]
fn render_writes_ppm_when_out_ends_in_ppm() {
    let (output, bytes) = render_hello_to("ppm");
    assert!(output.status.success(), "render failed: {output:?}");
    let bytes = bytes.expect("output PPM missing");
    assert!(bytes.starts_with(b"P6\n"), "output does not start with P6");
    assert!(
        stdout(&output).contains(".ppm ("),
        "summary names the file: {output:?}"
    );
}

#[test]
fn render_writes_bmp_when_out_ends_in_bmp() {
    let (output, bytes) = render_hello_to("bmp");
    assert!(output.status.success(), "render failed: {output:?}");
    let bytes = bytes.expect("output BMP missing");
    assert!(bytes.starts_with(b"BM"), "output does not start with BM");
}

#[test]
fn render_rejects_an_unknown_output_extension() {
    let (output, bytes) = render_hello_to("tiff");
    assert_eq!(output.status.code(), Some(1));
    assert!(bytes.is_none(), "a file was written despite the error");
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        err.contains("unsupported output format \"tiff\": use .png, .ppm or .bmp"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn render_warns_about_a_dropped_image_and_still_succeeds() {
    // The page's only content is an image whose filter pdfboss cannot
    // decode, so the PNG comes out blank. Rendering stays lenient -- exit 0,
    // file written -- but the warning and the annotated summary line are all
    // that stand between the user and a blank page reported as a clean one.
    let mut builder = pdfboss_testkit::PdfBuilder::new();
    builder.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    builder.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    builder.object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
         /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>",
    );
    builder.stream(4, "", b"q 100 0 0 100 0 0 cm /Im0 Do Q");
    builder.stream(
        5,
        "/Type /XObject /Subtype /Image /Width 8 /Height 8 /BitsPerComponent 1 \
         /ColorSpace /DeviceGray /Filter /Crypt",
        &[0; 8],
    );
    let dir = std::env::temp_dir();
    let pdf = dir.join(format!("pdfboss-cli-skip-{}.pdf", std::process::id()));
    let out = dir.join(format!("pdfboss-cli-skip-{}.png", std::process::id()));
    std::fs::write(&pdf, builder.build(1)).expect("write fixture");

    let output = pdfboss(&[
        "render",
        pdf.to_str().unwrap(),
        "--page",
        "1",
        "-o",
        out.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&pdf);
    let png_written = std::fs::read(&out).is_ok();
    let _ = std::fs::remove_file(&out);

    assert!(output.status.success(), "render failed: {output:?}");
    assert!(png_written, "no PNG written despite exit 0");
    let warnings = String::from_utf8_lossy(&output.stderr);
    assert!(
        warnings.contains("warning: page 1: 1 image skipped: unsupported filter /Crypt"),
        "no warning naming the filter in: {warnings}"
    );
    let text = stdout(&output);
    assert!(
        text.contains("[1 image skipped]"),
        "summary line does not mention the skip: {text}"
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

/// A missing `tui` target must fail before any terminal mode change: the
/// document open happens before `pdfboss_tui::run` (and therefore before
/// `enable_raw_mode`/`EnterAlternateScreen`) is ever called, so a
/// non-interactive run (stdin/stdout piped, not a tty, as `Command::output`
/// always gives us) exits 1 with a plain stderr message and no ANSI
/// alternate-screen escape sequence on stdout.
///
/// `cmd_tui` wraps the open error with the target spec (`{target}: {err}`,
/// the same shape `Input::open` uses for `json`/`hex`/`q`'s local path), so
/// the missing path must appear in stderr here too.
#[test]
fn tui_missing_file_exits_one_before_terminal_mode_change() {
    let output = pdfboss(&["tui", "definitely-not-here.pdf"]);
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(!err.is_empty(), "expected an error message");
    assert!(
        err.contains("definitely-not-here.pdf"),
        "path missing from: {err}"
    );
    assert!(
        output.stdout.is_empty(),
        "expected no stdout output (no terminal was ever entered): {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    // `\x1b[?1049h` is the alternate-screen-enter sequence `EnterAlternateScreen`
    // writes; its absence confirms raw mode / the alt screen was never touched.
    assert!(
        !output.stdout.windows(8).any(|w| w == b"\x1b[?1049h"),
        "terminal mode was changed before the open failed"
    );
}

/// A `tui` target that opens successfully but is run with no controlling
/// terminal at all must fail with a human-readable message instead of
/// surfacing crossterm's raw `enable_raw_mode` OS error ("Device not
/// configured" / "Inappropriate ioctl for device") verbatim.
#[cfg(unix)]
#[test]
fn tui_without_a_terminal_reports_a_friendly_error() {
    let file = fixture("hello.pdf");
    let output = pdfboss_without_a_terminal(&["tui", file.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "expected exit 1: {output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("interactive terminal"),
        "expected the friendly no-tty message, got: {err}"
    );
}

/// `json` and `tui` must both reject an encrypted document, not just
/// `info`/`text`/`render`/`obj`.
///
/// The fixture is a syntactically-plausible but *cryptographically
/// invalid* Standard-handler dict -- the same one
/// `pdfboss_aio::document::tests::encrypted_documents_are_rejected_at_open`
/// uses: its `/O` and `/U` strings are literal placeholder text, not real
/// password hashes, so they can never verify. That means this fixture does
/// not exercise "real" empty-password decryption (which sync `Document`
/// does support, per the README) on either side: sync `Document::load`
/// itself declines it with `Error::Encrypted` before any object is
/// decrypted (mirroring
/// `pdfboss_core::crypt::tests::document_load_rejects_when_password_does_not_verify`,
/// which pins that same reject-on-no-verify behavior for a real V2/R3
/// handler), and the async path rejects every encrypted document
/// unconditionally, real or not. So both commands are expected to fail
/// here for the same underlying reason, and this test honestly pins that
/// -- it is not proof that `json` opens *real* empty-password-encrypted
/// files (see `info`/`text`/`render`/`obj`'s own coverage for that).
#[test]
fn json_and_tui_both_reject_an_encrypted_document() {
    let mut b = pdfboss_testkit::PdfBuilder::new().trailer_extra("/Encrypt 9 0 R");
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
    b.object(
        9,
        "<< /Filter /Standard /V 1 /R 2 /O (dummydummydummydummydummydummyd) \
         /U (dummydummydummydummydummydummyd) /P -3904 >>",
    );
    let data = b.build(1);
    let path =
        std::env::temp_dir().join(format!("pdfboss-cli-encrypted-{}.pdf", std::process::id()));
    std::fs::write(&path, &data).expect("write encrypted fixture");

    let json_output = pdfboss(&["json", path.to_str().unwrap()]);
    let tui_output = pdfboss(&["tui", path.to_str().unwrap()]);
    std::fs::remove_file(&path).ok();

    assert_eq!(
        json_output.status.code(),
        Some(1),
        "json over an encrypted document must fail: {json_output:?}"
    );
    let json_err = String::from_utf8_lossy(&json_output.stderr);
    assert!(
        json_err.contains("encrypted documents are not supported"),
        "json's error does not mention encryption: {json_err}"
    );

    assert_eq!(
        tui_output.status.code(),
        Some(1),
        "tui over an encrypted document must fail: {tui_output:?}"
    );
    let tui_err = String::from_utf8_lossy(&tui_output.stderr);
    assert!(
        tui_err.contains("encrypted documents are not supported"),
        "tui's error does not mention encryption: {tui_err}"
    );
}
