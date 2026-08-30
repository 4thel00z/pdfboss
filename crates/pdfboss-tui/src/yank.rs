//! Yank menu: copy the selection as a q expression, shell command,
//! hexdump, raw bytes, pretty value, or object reference.

/// Fetched copies above this many bytes hand over a CLI command instead:
/// bigger payloads exceed what terminals and clipboards handle gracefully.
pub const CAP_BYTES: u64 = 1024 * 1024;

/// Wraps `text` in single quotes for POSIX shells, escaping embedded
/// single quotes as `'\''`.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The `pdfboss q` invocation running `query` over `target`.
pub fn q_command(target: &str, query: &str) -> String {
    format!("pdfboss q {} {}", shell_quote(target), shell_quote(query))
}

/// The `pdfboss hex` invocation dumping `selector` of `target`.
pub fn hex_command(target: &str, selector: &str) -> String {
    format!(
        "pdfboss hex {} {}",
        shell_quote(target),
        shell_quote(selector)
    )
}

/// xxd-style lines, 16 bytes wide split 8+8, offsets starting at `base`,
/// non-printable bytes shown as `.` in the trailing ascii column.
pub fn hexdump_text(bytes: &[u8], base: u64) -> String {
    let lines: Vec<String> = bytes
        .chunks(16)
        .enumerate()
        .map(|(index, chunk)| {
            let mut hex = String::new();
            for (position, byte) in chunk.iter().enumerate() {
                if position > 0 {
                    hex.push(' ');
                }
                if position == 8 {
                    hex.push(' ');
                }
                hex.push_str(&format!("{byte:02x}"));
            }
            let ascii: String = chunk
                .iter()
                .map(|byte| {
                    if (0x20..=0x7e).contains(byte) {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect();
            let offset = base + (index as u64) * 16;
            format!("{offset:08x}: {hex:<48} |{ascii}|")
        })
        .collect();
    lines.join("\n")
}

/// `18 B`, `4.0 KiB`, `1.5 MiB`.
pub fn human_size(len: u64) -> String {
    if len < 1024 {
        return format!("{len} B");
    }
    let kib = len as f64 / 1024.0;
    if kib < 1024.0 {
        return format!("{kib:.1} KiB");
    }
    format!("{:.1} MiB", kib / 1024.0)
}

/// What the second key of the yank menu copies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum YankTarget {
    /// The q expression addressing the selection, e.g. `.objects["12 0"]`.
    Query,
    /// The full shell command, e.g. `pdfboss q 'in.pdf' '.trailer'`.
    Command,
    /// A hexdump of the selection's bytes.
    Hexdump,
    /// The selection's raw bytes (UTF-8 lossy).
    Bytes,
    /// The pretty-printed element the inspector shows.
    Value,
    /// The object reference, e.g. `12 0 R`.
    ObjRef,
}

/// How fetched bytes become clipboard text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum YankFormat {
    /// xxd-style hexdump lines.
    Hexdump,
    /// The bytes themselves, UTF-8 lossy.
    Bytes,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(shell_quote("plain.pdf"), "'plain.pdf'");
        assert_eq!(shell_quote("o'clock.pdf"), "'o'\\''clock.pdf'");
    }

    #[test]
    fn hex_command_quotes_target_and_selector() {
        assert_eq!(
            hex_command("my file.pdf", "range:0-16"),
            "pdfboss hex 'my file.pdf' 'range:0-16'"
        );
    }

    #[test]
    fn hexdump_text_formats_sixteen_wide_from_the_base_offset() {
        let mut bytes: Vec<u8> = (b'A'..=b'P').collect();
        bytes.push(0x00);
        bytes.push(b'Q');
        assert_eq!(
            hexdump_text(&bytes, 0xf),
            "0000000f: 41 42 43 44 45 46 47 48  49 4a 4b 4c 4d 4e 4f 50 |ABCDEFGHIJKLMNOP|\n\
             0000001f: 00 51                                            |.Q|"
        );
    }

    #[test]
    fn human_size_scales_units() {
        assert_eq!(human_size(18), "18 B");
        assert_eq!(human_size(4 * 1024), "4.0 KiB");
        assert_eq!(human_size(3 * 1024 * 1024 / 2), "1.5 MiB");
    }
}
