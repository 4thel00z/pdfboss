//! hexyl-style hexdump: offset gutter, hex columns, ascii column, byte-class
//! coloring, and labeled region boundaries. Also home of the `pdfboss hex`
//! subcommand (wired in a later task).

use std::fmt::Write as _;
use std::io;

/// Hexdump options: bytes per row and ANSI coloring.
pub struct HexOpts {
    pub width: usize,
    pub color: bool,
}

impl Default for HexOpts {
    fn default() -> HexOpts {
        HexOpts {
            width: 16,
            color: false,
        }
    }
}

/// A labeled boundary printed when the dump reaches `offset`.
pub struct Mark {
    pub offset: u64,
    pub label: String,
}

/// Byte classes for coloring and the ascii column.
#[derive(Clone, Copy)]
enum ByteClass {
    Null,
    Printable,
    Whitespace,
    Other,
}

fn classify(b: u8) -> ByteClass {
    match b {
        0 => ByteClass::Null,
        b'\t' | b'\n' | b'\r' | 0x0B | 0x0C | b' ' => ByteClass::Whitespace,
        0x21..=0x7E => ByteClass::Printable,
        _ => ByteClass::Other,
    }
}

fn color_code(class: ByteClass) -> &'static str {
    match class {
        ByteClass::Null => "\x1b[90m",
        ByteClass::Printable => "\x1b[36m",
        ByteClass::Whitespace => "\x1b[32m",
        ByteClass::Other => "\x1b[33m",
    }
}

fn ascii_char(b: u8) -> char {
    match classify(b) {
        ByteClass::Printable => b as char,
        ByteClass::Whitespace if b == b' ' => ' ',
        ByteClass::Null | ByteClass::Whitespace | ByteClass::Other => '.',
    }
}

/// Dumps `bytes`, labeling the gutter as if the first byte sat at
/// `base_offset` in the file.
pub fn hexdump(
    w: &mut impl io::Write,
    bytes: &[u8],
    base_offset: u64,
    opts: &HexOpts,
) -> io::Result<()> {
    hexdump_marked(w, bytes, base_offset, opts, &[])
}

/// Like [`hexdump`], plus labeled boundary lines: before the row containing a
/// mark's offset a `── label ──` line is emitted (a mark at exactly the end
/// offset prints after the last row). `marks` must be sorted by offset; marks
/// outside `base_offset..=base_offset + bytes.len()` are dropped.
pub fn hexdump_marked(
    w: &mut impl io::Write,
    bytes: &[u8],
    base_offset: u64,
    opts: &HexOpts,
    marks: &[Mark],
) -> io::Result<()> {
    let width = opts.width.max(1);
    let end_offset = base_offset + bytes.len() as u64;
    let mut marks = marks
        .iter()
        .filter(|m| m.offset >= base_offset && m.offset <= end_offset)
        .peekable();
    let mut row_start = 0usize;
    while row_start < bytes.len() {
        let row_end = (row_start + width).min(bytes.len());
        let row_off_end = base_offset + row_end as u64;
        while marks.peek().is_some_and(|m| m.offset < row_off_end) {
            writeln!(w, "── {} ──", marks.next().expect("peeked").label)?;
        }
        write_row(
            w,
            &bytes[row_start..row_end],
            base_offset + row_start as u64,
            width,
            opts.color,
        )?;
        row_start = row_end;
    }
    for mark in marks {
        writeln!(w, "── {} ──", mark.label)?;
    }
    Ok(())
}

fn write_row(
    w: &mut impl io::Write,
    row: &[u8],
    offset: u64,
    width: usize,
    color: bool,
) -> io::Result<()> {
    const RESET: &str = "\x1b[0m";
    let mut hex = String::new();
    let mut ascii = String::new();
    for (i, &b) in row.iter().enumerate() {
        if i > 0 && i % 8 == 0 {
            hex.push(' ');
        }
        if color {
            let code = color_code(classify(b));
            let _ = write!(hex, "{code}{b:02x}{RESET} ");
            let _ = write!(ascii, "{code}{}{RESET}", ascii_char(b));
        } else {
            let _ = write!(hex, "{b:02x} ");
            ascii.push(ascii_char(b));
        }
    }
    for i in row.len()..width {
        if i > 0 && i % 8 == 0 {
            hex.push(' ');
        }
        hex.push_str("   ");
    }
    writeln!(w, "{offset:08x}  {hex} |{ascii}|")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(bytes: &[u8], base: u64, opts: &HexOpts) -> String {
        let mut out = Vec::new();
        hexdump(&mut out, bytes, base, opts).expect("write to Vec cannot fail");
        String::from_utf8(out).expect("dump is valid text")
    }

    fn plain(width: usize) -> HexOpts {
        HexOpts {
            width,
            color: false,
        }
    }

    #[test]
    fn full_row_width_4() {
        assert_eq!(
            dump(b"ABCD", 0, &plain(4)),
            "00000000  41 42 43 44  |ABCD|\n"
        );
    }

    #[test]
    fn partial_row_pads_hex_column() {
        let expected = format!(
            "00000000  41 42 43 44  |ABCD|\n00000004  45{}|E|\n",
            " ".repeat(11)
        );
        assert_eq!(dump(b"ABCDE", 0, &plain(4)), expected);
    }

    #[test]
    fn sixteen_wide_rows_group_by_eight_and_use_the_base_offset() {
        assert_eq!(
            dump(b"0123456789abcdef", 0x1a40, &plain(16)),
            "00001a40  30 31 32 33 34 35 36 37  38 39 61 62 63 64 65 66  |0123456789abcdef|\n"
        );
    }

    #[test]
    fn second_row_offset_advances_by_width() {
        let out = dump(&[0u8; 17], 0, &plain(16));
        let second = out.lines().nth(1).expect("two rows");
        assert!(second.starts_with("00000010  "), "wrong offset: {second}");
    }

    #[test]
    fn ascii_column_shows_dots_for_non_printables_and_keeps_spaces() {
        assert_eq!(
            dump(&[0x00, b'\n', b'A', b' ', 0xff], 0, &plain(8)),
            format!("00000000  00 0a 41 20 ff{}|..A .|\n", " ".repeat(11))
        );
    }

    #[test]
    fn empty_input_dumps_nothing() {
        assert_eq!(dump(b"", 0, &plain(16)), "");
    }

    #[test]
    fn byte_classes_get_their_colors() {
        let colored = dump(
            &[0x00, b'\n', b'A', 0xff],
            0,
            &HexOpts {
                width: 4,
                color: true,
            },
        );
        assert!(
            colored.contains("\x1b[90m00\x1b[0m"),
            "null not bright black: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[32m0a\x1b[0m"),
            "whitespace not green: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[36m41\x1b[0m"),
            "printable not cyan: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[33mff\x1b[0m"),
            "other not yellow: {colored:?}"
        );
        assert!(
            colored.contains("\x1b[36mA\x1b[0m"),
            "ascii column uncolored: {colored:?}"
        );
    }

    #[test]
    fn marks_print_before_the_row_containing_their_offset() {
        let marks = vec![
            Mark {
                offset: 0,
                label: "header".to_string(),
            },
            Mark {
                offset: 5,
                label: "obj 1 0".to_string(),
            },
        ];
        let mut out = Vec::new();
        hexdump_marked(&mut out, b"ABCDEFGH", 0, &plain(4), &marks).expect("writes");
        assert_eq!(
            String::from_utf8(out).expect("text"),
            "── header ──\n00000000  41 42 43 44  |ABCD|\n── obj 1 0 ──\n00000004  45 46 47 48  |EFGH|\n"
        );
    }

    #[test]
    fn marks_outside_the_dumped_range_are_dropped() {
        let marks = vec![
            Mark {
                offset: 2,
                label: "before".to_string(),
            },
            Mark {
                offset: 100,
                label: "after".to_string(),
            },
        ];
        let mut out = Vec::new();
        hexdump_marked(&mut out, b"ABCD", 4, &plain(4), &marks).expect("writes");
        let text = String::from_utf8(out).expect("text");
        assert!(!text.contains("before"), "mark before range kept: {text}");
        assert!(!text.contains("after"), "mark past range kept: {text}");
    }

    #[test]
    fn mark_at_end_offset_prints_after_the_last_row() {
        let marks = vec![Mark {
            offset: 4,
            label: "eof".to_string(),
        }];
        let mut out = Vec::new();
        hexdump_marked(&mut out, b"ABCD", 0, &plain(4), &marks).expect("writes");
        assert_eq!(
            String::from_utf8(out).expect("text"),
            "00000000  41 42 43 44  |ABCD|\n── eof ──\n"
        );
    }
}
