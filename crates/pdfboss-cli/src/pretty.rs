//! Human-readable, indented pretty-printing of PDF objects.

use std::fmt::Write as _;

use pdfboss_core::object::decode_text_string;
use pdfboss_core::{Dict, Name, Object};

/// Maximum rendered length for an array to stay on a single line.
const INLINE_ARRAY_LIMIT: usize = 60;

/// Formats any object as an indented, multi-line string.
pub fn format_object(obj: &Object) -> String {
    let mut out = String::new();
    write_obj(obj, 0, &mut out);
    out
}

/// Formats a dictionary as an indented, multi-line string.
pub fn format_dict(dict: &Dict) -> String {
    let mut out = String::new();
    write_dict(dict, 0, &mut out);
    out
}

fn push_indent(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_obj(obj: &Object, level: usize, out: &mut String) {
    match obj {
        Object::Null => out.push_str("null"),
        Object::Bool(b) => {
            let _ = write!(out, "{b}");
        }
        Object::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Object::Real(r) => {
            let _ = write!(out, "{r}");
        }
        Object::String(bytes) => write_string(bytes, out),
        Object::Name(name) => write_name(name, out),
        Object::Array(items) => write_array(items, level, out),
        Object::Dict(dict) => write_dict(dict, level, out),
        Object::Stream(s) => {
            write_dict(&s.dict, level, out);
            let _ = write!(out, "\nstream <{} raw bytes>", s.data.len());
        }
        Object::Ref(r) => {
            let _ = write!(out, "{} {} R", r.num, r.gen);
        }
    }
}

fn is_scalar(obj: &Object) -> bool {
    !matches!(obj, Object::Array(_) | Object::Dict(_) | Object::Stream(_))
}

fn write_array(items: &[Object], level: usize, out: &mut String) {
    if items.is_empty() {
        out.push_str("[]");
        return;
    }
    if items.iter().all(is_scalar) {
        let mut inline = String::new();
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                inline.push(' ');
            }
            write_obj(item, 0, &mut inline);
        }
        if inline.len() <= INLINE_ARRAY_LIMIT {
            let _ = write!(out, "[{inline}]");
            return;
        }
    }
    out.push('[');
    for item in items {
        out.push('\n');
        push_indent(level + 1, out);
        write_obj(item, level + 1, out);
    }
    out.push('\n');
    push_indent(level, out);
    out.push(']');
}

fn write_dict(dict: &Dict, level: usize, out: &mut String) {
    if dict.is_empty() {
        out.push_str("<< >>");
        return;
    }
    let mut entries: Vec<(&Name, &Object)> = dict.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    out.push_str("<<");
    for (name, value) in entries {
        out.push('\n');
        push_indent(level + 1, out);
        write_name(name, out);
        out.push(' ');
        write_obj(value, level + 1, out);
    }
    out.push('\n');
    push_indent(level, out);
    out.push_str(">>");
}

fn write_name(name: &Name, out: &mut String) {
    out.push('/');
    for &b in name.0.as_bytes() {
        let c = b as char;
        let delimiter = matches!(
            c,
            '(' | ')' | '<' | '>' | '[' | ']' | '{' | '}' | '/' | '%' | '#'
        );
        if b.is_ascii_graphic() && !delimiter {
            out.push(c);
        } else {
            let _ = write!(out, "#{b:02X}");
        }
    }
}

fn write_string(bytes: &[u8], out: &mut String) {
    let text = decode_text_string(bytes);
    out.push('(');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(')');
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::ObjRef;

    fn name(s: &str) -> Name {
        Name(s.to_string())
    }

    #[test]
    fn scalars() {
        assert_eq!(format_object(&Object::Null), "null");
        assert_eq!(format_object(&Object::Bool(true)), "true");
        assert_eq!(format_object(&Object::Int(-42)), "-42");
        assert_eq!(format_object(&Object::Real(1.5)), "1.5");
        assert_eq!(
            format_object(&Object::Ref(ObjRef { num: 3, gen: 1 })),
            "3 1 R"
        );
    }

    #[test]
    fn strings_escape_specials() {
        let obj = Object::String(b"a(b)\\c".to_vec());
        assert_eq!(format_object(&obj), "(a\\(b\\)\\\\c)");
    }

    #[test]
    fn names_escape_irregular_bytes() {
        assert_eq!(format_object(&Object::Name(name("Type"))), "/Type");
        assert_eq!(format_object(&Object::Name(name("A B"))), "/A#20B");
        assert_eq!(format_object(&Object::Name(name("a/b"))), "/a#2Fb");
    }

    #[test]
    fn short_scalar_array_is_inline() {
        let obj = Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(612)]);
        assert_eq!(format_object(&obj), "[0 0 612]");
    }

    #[test]
    fn nested_array_is_multiline() {
        let obj = Object::Array(vec![Object::Int(1), Object::Array(vec![Object::Int(2)])]);
        assert_eq!(format_object(&obj), "[\n  1\n  [2]\n]");
    }

    #[test]
    fn empty_containers() {
        assert_eq!(format_object(&Object::Array(vec![])), "[]");
        assert_eq!(format_dict(&Dict::new()), "<< >>");
    }

    #[test]
    fn dict_is_sorted_and_indented() {
        let mut d = Dict::new();
        d.insert(name("Type"), Object::Name(name("Page")));
        d.insert(name("Count"), Object::Int(3));
        assert_eq!(format_dict(&d), "<<\n  /Count 3\n  /Type /Page\n>>");
    }

    #[test]
    fn nested_dict_indents_two_levels() {
        let mut inner = Dict::new();
        inner.insert(name("N"), Object::Int(1));
        let mut outer = Dict::new();
        outer.insert(name("Inner"), Object::Dict(inner));
        assert_eq!(format_dict(&outer), "<<\n  /Inner <<\n    /N 1\n  >>\n>>");
    }
}
