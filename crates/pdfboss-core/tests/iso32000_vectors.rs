//! Differential vectors for the ASCIIHexDecode (ISO 32000-1 §7.4.2),
//! ASCII85Decode (ISO 32000-1 §7.4.3) and RunLengthDecode (ISO 32000-1 §7.4.5)
//! filters. The expected outputs come from the executable Lean references in
//! `iso32000/Iso32000/Reference/`, whose decode-after-encode theorems the Lean
//! kernel has checked; `make iso32000-vectors` rewrites the files.

use pdfboss_core::filters::{ascii85, ascii_hex, run_length};
use std::path::PathBuf;

struct Vector {
    name: String,
    input: Vec<u8>,
    expected: Option<Vec<u8>>,
}

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).expect("hex digits are ASCII");
            u8::from_str_radix(digits, 16).expect("two hex digits")
        })
        .collect()
}

fn vectors(file: &str) -> Vec<Vector> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "vectors",
        "iso32000",
        file,
    ]
    .iter()
    .collect();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let parsed: Vec<Vector> = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next().expect("vector name").to_string();
            let input = unhex(fields.next().expect("vector input"));
            let expected = match fields.next().expect("vector expectation") {
                "ERR" => None,
                hex => Some(unhex(hex)),
            };
            Vector {
                name,
                input,
                expected,
            }
        })
        .collect();
    assert!(
        parsed.len() >= 30,
        "{file} holds only {} vectors",
        parsed.len()
    );
    parsed
}

fn check(file: &str, decode: fn(&[u8]) -> pdfboss_core::Result<Vec<u8>>) {
    for vector in vectors(file) {
        match (&vector.expected, decode(&vector.input)) {
            (Some(expected), Ok(actual)) => {
                assert_eq!(&actual, expected, "{}: decoded bytes differ", vector.name)
            }
            (Some(_), Err(e)) => panic!("{}: pdfboss rejected a valid input: {e}", vector.name),
            (None, Ok(actual)) => panic!(
                "{}: pdfboss accepted an input the reference rejects, giving {} bytes",
                vector.name,
                actual.len()
            ),
            (None, Err(_)) => {}
        }
    }
}

// Covers ISO 32000-1 §7.4.2.
#[test]
fn ascii_hex_matches_the_lean_reference() {
    check("ascii_hex.txt", ascii_hex::decode);
}

// Covers ISO 32000-1 §7.4.3.
#[test]
fn ascii85_matches_the_lean_reference() {
    check("ascii85.txt", ascii85::decode);
}

// Covers ISO 32000-1 §7.4.5.
#[test]
fn run_length_matches_the_lean_reference() {
    check("run_length.txt", run_length::decode);
}
