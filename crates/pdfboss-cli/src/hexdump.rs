//! hexyl-style hexdump: offset gutter, hex columns, ascii column, byte-class
//! coloring, and labeled region boundaries. Also home of the `pdfboss hex`
//! subcommand: selector parsing/resolution, `--annotate` boundary marks, and
//! `cmd_hex` itself.

use std::fmt::Write as _;
use std::io;

use pdfboss_core::elements::{Element, ElementOpts, Span, XrefKind};

use crate::input::{use_color, Input};

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

/// What part of the file `pdfboss hex` dumps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Selector {
    WholeFile,
    Header,
    Trailer,
    Obj { num: u32, gen: Option<u16> },
    Xref { index: usize },
    Range { start: u64, end: u64 },
}

/// Parses `obj:12` / `obj:12,0` / `header` / `xref:0` / `trailer` /
/// `range:0x1A40-0x1B02` (offsets decimal or 0x-prefixed hex).
pub fn parse_selector(s: &str) -> Result<Selector, String> {
    if s == "header" {
        return Ok(Selector::Header);
    }
    if s == "trailer" {
        return Ok(Selector::Trailer);
    }
    if let Some(rest) = s.strip_prefix("obj:") {
        let (num, gen) = match rest.split_once(',') {
            None => (rest, None),
            Some((num, gen)) => (num, Some(gen)),
        };
        let num: u32 = num
            .trim()
            .parse()
            .map_err(|_| format!("bad object number in selector {s:?}"))?;
        let gen: Option<u16> = match gen {
            None => None,
            Some(gen) => Some(
                gen.trim()
                    .parse()
                    .map_err(|_| format!("bad generation in selector {s:?}"))?,
            ),
        };
        return Ok(Selector::Obj { num, gen });
    }
    if let Some(rest) = s.strip_prefix("xref:") {
        let index: usize = rest
            .trim()
            .parse()
            .map_err(|_| format!("bad xref index in selector {s:?}"))?;
        return Ok(Selector::Xref { index });
    }
    if let Some(rest) = s.strip_prefix("range:") {
        let (start, end) = rest.split_once('-').ok_or_else(|| {
            format!("range selector must look like range:0x1A40-0x1B02, got {s:?}")
        })?;
        let start = parse_offset(start)?;
        let end = parse_offset(end)?;
        if end < start {
            return Err(format!("range end {end:#x} precedes start {start:#x}"));
        }
        return Ok(Selector::Range { start, end });
    }
    Err(format!(
        "unknown selector {s:?}: expected obj:N[,G], header, xref:N, trailer, or range:START-END"
    ))
}

fn parse_offset(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|_| format!("bad offset {s:?}: expected decimal or 0x-prefixed hex"))
}

/// Maps a selector to a physical byte span using the element sequence.
/// For object-stream members `obj:` resolves to the container's span (that is
/// where the bytes live in the file). `xref:N` indexes sections in the order
/// they are yielded — xref-chain order, newest first (`xref:0` = newest).
pub fn resolve_selector(
    sel: &Selector,
    elements: &[Element],
    file_len: u64,
) -> Result<Span, String> {
    match sel {
        Selector::WholeFile => Ok(Span {
            start: 0,
            end: file_len,
        }),
        Selector::Range { start, end } => Ok(Span {
            start: *start,
            end: *end,
        }),
        Selector::Header => elements
            .iter()
            .find_map(|element| match element {
                Element::Header { span, .. } => Some(*span),
                _ => None,
            })
            .ok_or_else(|| "no header element found".to_string()),
        // The Trailer element is emitted once per document (merged dict, span
        // of the newest trailer region), so the first match is the only one.
        Selector::Trailer => elements
            .iter()
            .find_map(|element| match element {
                Element::Trailer { span, .. } => Some(*span),
                _ => None,
            })
            .ok_or_else(|| "no trailer element found".to_string()),
        Selector::Xref { index } => elements
            .iter()
            .filter_map(|element| match element {
                Element::XrefSection { span, .. } => Some(*span),
                _ => None,
            })
            .nth(*index)
            .ok_or_else(|| format!("no xref section {index}")),
        Selector::Obj { num, gen } => elements
            .iter()
            .find_map(|element| match element {
                Element::IndirectObject { r, span, .. }
                    if r.num == *num && gen.is_none_or(|g| g == r.gen) =>
                {
                    Some(*span)
                }
                _ => None,
            })
            .ok_or_else(|| match gen {
                Some(gen) => format!("object {num} {gen} not found"),
                None => format!("object {num} not found"),
            }),
    }
}

/// Boundary marks for `--annotate`: one labeled mark at every physical
/// element's span start, sorted by offset. The sort is load-bearing: xref
/// sections stream in chain order (newest first), not ascending file order,
/// and `hexdump_marked` requires offset-sorted marks (its own doc comment
/// says so but does not enforce it). Object-stream members are skipped (they
/// would duplicate their container's boundary).
pub fn element_marks(elements: &[Element]) -> Vec<Mark> {
    let mut marks: Vec<Mark> = elements
        .iter()
        .filter_map(|element| match element {
            Element::Header { span, .. } => Some(Mark {
                offset: span.start,
                label: "header".to_string(),
            }),
            Element::IndirectObject {
                in_objstm: Some(_), ..
            } => None,
            Element::IndirectObject { r, span, .. } => Some(Mark {
                offset: span.start,
                label: format!("obj {} {}", r.num, r.gen),
            }),
            Element::XrefSection {
                kind,
                span,
                entries,
            } => {
                let kind = match kind {
                    XrefKind::Table => "table",
                    XrefKind::Stream => "stream",
                };
                Some(Mark {
                    offset: span.start,
                    label: format!("xref {kind} ({entries} entries)"),
                })
            }
            Element::Trailer { span, .. } => Some(Mark {
                offset: span.start,
                label: "trailer".to_string(),
            }),
            Element::StartXref { span, .. } => Some(Mark {
                offset: span.start,
                label: "startxref".to_string(),
            }),
            Element::Eof { span } => Some(Mark {
                offset: span.start,
                label: "eof".to_string(),
            }),
            Element::Page { .. }
            | Element::Font { .. }
            | Element::Image { .. }
            | Element::Annotation { .. }
            | Element::ContentOp { .. } => None,
        })
        .collect();
    marks.sort_by_key(|m| m.offset);
    marks
}

/// `pdfboss hex <file-or-url> [selector] [--annotate] [--width N]`.
pub fn cmd_hex(
    input_spec: &str,
    selector: Option<&str>,
    annotate: bool,
    width: usize,
) -> Result<(), String> {
    if width == 0 {
        return Err("--width must be at least 1".to_string());
    }
    let sel = match selector {
        Some(s) => parse_selector(s)?,
        None => Selector::WholeFile,
    };
    let input = Input::open(input_spec)?;
    let opts = ElementOpts {
        physical: true,
        logical: false,
        pages: None,
        content_ops: false,
    };
    let elements = input.collect_elements(opts);
    let file_len = input.file_len();
    let span = resolve_selector(&sel, &elements, file_len)?;
    // `Input::read_span` diverges by backend on out-of-range spans: the local
    // fast path errors, the remote/aio path silently clamps to the file
    // length. Validate here so both backends fail the same way for a
    // resolved span that runs past the end of the file.
    if span.start > file_len || span.end > file_len {
        return Err(format!(
            "selector resolved to {}..{}, which lies outside the file ({file_len} bytes)",
            span.start, span.end
        ));
    }
    let bytes = input.read_span(span)?;
    let hex_opts = HexOpts {
        width,
        color: use_color(),
    };
    let stdout = io::stdout();
    let mut w = io::BufWriter::new(stdout.lock());
    if annotate {
        let marks = element_marks(&elements);
        hexdump_marked(&mut w, &bytes, span.start, &hex_opts, &marks).map_err(|e| e.to_string())
    } else {
        // Equivalent to `hexdump_marked(.., &[])` by definition — calling the
        // plain wrapper here (rather than always going through
        // `hexdump_marked` with an empty marks slice) keeps it a real,
        // reachable part of the binary now that `mod hexdump` no longer
        // blanket-allows dead code.
        hexdump(&mut w, &bytes, span.start, &hex_opts).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{Dict, ObjRef, Object};

    fn sample_elements() -> Vec<Element> {
        vec![
            Element::Header {
                version: (1, 7),
                span: Span { start: 0, end: 15 },
            },
            Element::IndirectObject {
                r: ObjRef { num: 1, gen: 0 },
                object: Object::Null,
                span: Span { start: 15, end: 60 },
                in_objstm: None,
            },
            Element::IndirectObject {
                r: ObjRef { num: 2, gen: 3 },
                object: Object::Null,
                span: Span {
                    start: 60,
                    end: 120,
                },
                in_objstm: Some((ObjRef { num: 9, gen: 0 }, Span { start: 4, end: 20 })),
            },
            Element::XrefSection {
                kind: XrefKind::Table,
                span: Span {
                    start: 120,
                    end: 180,
                },
                entries: 3,
            },
            Element::Trailer {
                dict: Dict::new(),
                span: Span {
                    start: 180,
                    end: 220,
                },
            },
            Element::StartXref {
                offset: 120,
                span: Span {
                    start: 220,
                    end: 235,
                },
            },
            Element::Eof {
                span: Span {
                    start: 235,
                    end: 241,
                },
            },
        ]
    }

    #[test]
    fn selectors_parse() {
        assert!(matches!(parse_selector("header"), Ok(Selector::Header)));
        assert!(matches!(parse_selector("trailer"), Ok(Selector::Trailer)));
        assert!(matches!(
            parse_selector("obj:12"),
            Ok(Selector::Obj { num: 12, gen: None })
        ));
        assert!(matches!(
            parse_selector("obj:12,0"),
            Ok(Selector::Obj {
                num: 12,
                gen: Some(0)
            })
        ));
        assert!(matches!(
            parse_selector("xref:0"),
            Ok(Selector::Xref { index: 0 })
        ));
        assert!(matches!(
            parse_selector("range:0x1A40-0x1B02"),
            Ok(Selector::Range {
                start: 0x1a40,
                end: 0x1b02
            })
        ));
        assert!(matches!(
            parse_selector("range:0-16"),
            Ok(Selector::Range { start: 0, end: 16 })
        ));
    }

    #[test]
    fn bad_selectors_error_with_guidance() {
        assert!(parse_selector("obj:x").is_err());
        assert!(parse_selector("obj:1,y").is_err());
        assert!(parse_selector("xref:z").is_err());
        assert!(parse_selector("range:5").is_err());
        assert!(parse_selector("range:9-5").is_err());
        let err = parse_selector("bogus").expect_err("unknown selector");
        assert!(err.contains("obj:N"), "no guidance in: {err}");
    }

    #[test]
    fn selectors_resolve_to_spans() {
        let elements = sample_elements();
        let resolve = |sel: &Selector| resolve_selector(sel, &elements, 241).expect("resolves");
        assert_eq!(resolve(&Selector::WholeFile), Span { start: 0, end: 241 });
        assert_eq!(resolve(&Selector::Header), Span { start: 0, end: 15 });
        assert_eq!(
            resolve(&Selector::Trailer),
            Span {
                start: 180,
                end: 220
            }
        );
        assert_eq!(
            resolve(&Selector::Obj { num: 1, gen: None }),
            Span { start: 15, end: 60 }
        );
        assert_eq!(
            resolve(&Selector::Obj {
                num: 2,
                gen: Some(3)
            }),
            Span {
                start: 60,
                end: 120
            }
        );
        assert_eq!(
            resolve(&Selector::Xref { index: 0 }),
            Span {
                start: 120,
                end: 180
            }
        );
        assert_eq!(
            resolve(&Selector::Range { start: 3, end: 9 }),
            Span { start: 3, end: 9 }
        );
    }

    #[test]
    fn unresolvable_selectors_report_what_was_asked() {
        let elements = sample_elements();
        assert!(resolve_selector(&Selector::Obj { num: 99, gen: None }, &elements, 241).is_err());
        assert!(resolve_selector(
            &Selector::Obj {
                num: 1,
                gen: Some(7)
            },
            &elements,
            241
        )
        .is_err());
        assert!(resolve_selector(&Selector::Xref { index: 5 }, &elements, 241).is_err());
        assert!(resolve_selector(&Selector::Header, &[], 241).is_err());
        assert!(resolve_selector(&Selector::Trailer, &[], 241).is_err());
    }

    #[test]
    fn element_marks_label_physical_boundaries_in_offset_order() {
        let marks = element_marks(&sample_elements());
        let labels: Vec<&str> = marks.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "header",
                "obj 1 0",
                "xref table (3 entries)",
                "trailer",
                "startxref",
                "eof"
            ]
        );
        let offsets: Vec<u64> = marks.iter().map(|m| m.offset).collect();
        assert_eq!(offsets, vec![0, 15, 120, 180, 220, 235]);
    }

    #[test]
    fn objstm_members_produce_no_marks() {
        let marks = element_marks(&sample_elements());
        assert!(
            marks.iter().all(|m| !m.label.contains("obj 2 3")),
            "objstm member must not be labeled"
        );
    }

    fn fixture(name: &str) -> String {
        format!(
            "{}/../../tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        )
    }

    // `Input::read_span` errors on out-of-range spans for local files but
    // silently clamps for remote ones (see `pdfboss_aio::AsyncDocument::read_span`).
    // `cmd_hex` must normalize this: both backends should fail the same clear
    // way for a selector that resolves past the end of the file.
    #[test]
    fn cmd_hex_out_of_range_range_selector_errors_with_file_length_on_local_file() {
        let path = fixture("hello.pdf");
        let len = std::fs::metadata(&path).expect("fixture exists").len();
        let selector = format!("range:0-{}", len + 1000);
        let err =
            cmd_hex(&path, Some(&selector), false, 16).expect_err("out-of-range range must fail");
        assert!(
            err.contains(&len.to_string()),
            "error does not mention file length {len}: {err}"
        );
    }

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
