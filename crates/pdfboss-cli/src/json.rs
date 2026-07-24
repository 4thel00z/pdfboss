//! `pdfboss json`: dump the whole document as a pretty-printed JSON value
//! tree, plus the JSON writer shared with `pdfboss q`.

use pdfboss_core::Stream;
use serde_json::Value;

use crate::input::{use_color, Input};
use crate::q::value::{build_tree, TreeFlags};

/// `pdfboss json <file-or-url> [--raw|--decode] [--pages ..] [--no-logical]
/// [--content-ops]`: dumps the full value tree for piping to external tools.
pub fn cmd_json(input_spec: &str, flags: &TreeFlags) -> Result<(), String> {
    let input = Input::open(input_spec)?;
    let opts = flags.element_opts()?;
    let elements = input.collect_elements(opts);
    let mut decode = |s: &Stream| input.decode_stream(s);
    let tree = build_tree(
        &elements,
        flags.stream_data(),
        flags.content_ops,
        &mut decode,
    );
    let mut text = String::new();
    write_json_pretty(&mut text, &tree, 0, use_color());
    println!("{text}");
    Ok(())
}

/// Two-space-indented JSON with optional ANSI coloring: keys cyan, strings
/// green, numbers yellow, booleans/null magenta. The uncolored layout is the
/// stable wire format golden tests pin.
pub fn write_json_pretty(out: &mut String, v: &Value, indent: usize, color: bool) {
    const KEY: &str = "\x1b[36m";
    const STR: &str = "\x1b[32m";
    const NUM: &str = "\x1b[33m";
    const LIT: &str = "\x1b[35m";
    const RESET: &str = "\x1b[0m";
    let paint = |out: &mut String, code: &str, text: &str| {
        if color {
            out.push_str(code);
            out.push_str(text);
            out.push_str(RESET);
        } else {
            out.push_str(text);
        }
    };
    match v {
        Value::Null => paint(out, LIT, "null"),
        Value::Bool(b) => paint(out, LIT, if *b { "true" } else { "false" }),
        Value::Number(n) => paint(out, NUM, &n.to_string()),
        Value::String(s) => {
            let quoted = serde_json::to_string(s).expect("strings always serialize");
            paint(out, STR, &quoted);
        }
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                push_indent(out, indent + 1);
                write_json_pretty(out, item, indent + 1, color);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (key, value)) in map.iter().enumerate() {
                push_indent(out, indent + 1);
                let quoted = serde_json::to_string(key).expect("strings always serialize");
                paint(out, KEY, &quoted);
                out.push_str(": ");
                write_json_pretty(out, value, indent + 1, color);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, indent);
            out.push('}');
        }
    }
}

fn push_indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plain(v: &Value) -> String {
        let mut out = String::new();
        write_json_pretty(&mut out, v, 0, false);
        out
    }

    #[test]
    fn scalars_print_bare() {
        assert_eq!(plain(&json!(null)), "null");
        assert_eq!(plain(&json!(true)), "true");
        assert_eq!(plain(&json!(42)), "42");
        assert_eq!(plain(&json!(1.5)), "1.5");
        assert_eq!(plain(&json!("a\"b")), r#""a\"b""#);
    }

    #[test]
    fn empty_containers_stay_inline() {
        assert_eq!(plain(&json!([])), "[]");
        assert_eq!(plain(&json!({})), "{}");
    }

    #[test]
    fn pretty_prints_nested_containers_two_space_indented() {
        let v = json!({"a": [1, 2], "b": {"c": "x"}});
        assert_eq!(
            plain(&v),
            "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {\n    \"c\": \"x\"\n  }\n}"
        );
    }

    #[test]
    fn color_paints_keys_strings_numbers_and_literals() {
        let v = json!({"k": ["s", 7, true, null]});
        let mut out = String::new();
        write_json_pretty(&mut out, &v, 0, true);
        assert!(
            out.contains("\x1b[36m\"k\"\x1b[0m"),
            "key not cyan: {out:?}"
        );
        assert!(
            out.contains("\x1b[32m\"s\"\x1b[0m"),
            "string not green: {out:?}"
        );
        assert!(
            out.contains("\x1b[33m7\x1b[0m"),
            "number not yellow: {out:?}"
        );
        assert!(
            out.contains("\x1b[35mtrue\x1b[0m"),
            "bool not magenta: {out:?}"
        );
        assert!(
            out.contains("\x1b[35mnull\x1b[0m"),
            "null not magenta: {out:?}"
        );
    }

    #[test]
    fn uncolored_output_carries_no_escapes() {
        let v = json!({"k": [1, "s"]});
        assert!(!plain(&v).contains('\x1b'));
    }
}
