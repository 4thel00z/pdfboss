//! Shared helpers for the explorer subcommand integration tests.

#![allow(dead_code)] // each test binary uses a subset

use std::path::PathBuf;
use std::process::{Command, Output};

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Runs the pdfboss binary with `NO_COLOR` set (belt and braces: piped
/// output is already colorless, but golden comparisons must never depend on
/// the environment).
pub fn pdfboss(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfboss"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to launch pdfboss binary")
}

pub fn stdout_str(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Removes ANSI CSI escape sequences (`ESC [ … <alpha>`).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Compares `actual` against `tests/golden/<name>`. Bless (re)creates the
/// file when `UPDATE_GOLDENS` is set; review the diff before committing.
pub fn assert_golden(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().expect("golden files live in a directory"))
            .expect("create golden dir");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "golden file {} missing; bless with UPDATE_GOLDENS=1",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "output differs from {}; review and re-bless with UPDATE_GOLDENS=1",
        path.display()
    );
}
