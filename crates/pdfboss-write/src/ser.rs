//! COS object serialization: `Object` values to PDF syntax bytes
//! (ISO 32000 §7.3). Output is deterministic — dictionary keys are emitted
//! in sorted order regardless of insertion order.
//!
//! Streams are deliberately absent here: a stream is only legal as an
//! indirect object and its `/Length` bookkeeping belongs to the
//! [`Writer`](crate::Writer), so a nested `Object::Stream` is an error.

use pdfboss_core::{Dict, Name, Object};

use crate::error::{Error, Result};

/// Serializes any non-stream object into `out`.
///
/// Numbers are written without exponents; reals trim trailing zeros and
/// never produce `-0`. Non-finite reals serialize as `0` (PDF has no
/// representation for them).
pub fn serialize_object(obj: &Object, out: &mut Vec<u8>) -> Result<()> {
    match obj {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(true) => out.extend_from_slice(b"true"),
        Object::Bool(false) => out.extend_from_slice(b"false"),
        Object::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => write_real(*r, out),
        Object::String(bytes) => write_string(bytes, out),
        Object::Name(n) => {
            nul_free(n)?;
            write_name(&n.0, out);
        }
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                serialize_object(item, out)?;
            }
            out.push(b']');
        }
        Object::Dict(d) => serialize_dict(d, out)?,
        Object::Ref(r) => {
            out.extend_from_slice(r.num.to_string().as_bytes());
            out.push(b' ');
            out.extend_from_slice(r.gen.to_string().as_bytes());
            out.extend_from_slice(b" R");
        }
        Object::Stream(_) => return Err(Error::NestedStream),
    }
    Ok(())
}

/// Serializes a dictionary with `<< … >>` delimiters, keys sorted
/// bytewise for deterministic output.
pub fn serialize_dict(dict: &Dict, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(b"<<");
    let mut entries: Vec<(&Name, &Object)> = dict.iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (key, value) in entries {
        nul_free(key)?;
        out.push(b' ');
        write_name(&key.0, out);
        out.push(b' ');
        serialize_object(value, out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

/// Writes a name object with its leading solidus, escaping every byte
/// outside the regular range as `#xx` (delimiters, whitespace, `#` itself,
/// and anything outside `0x21..=0x7E`).
pub fn write_name(name: &str, out: &mut Vec<u8>) {
    out.push(b'/');
    for &byte in name.as_bytes() {
        if (0x21..=0x7E).contains(&byte) && !b"()<>[]{}/%#".contains(&byte) {
            out.push(byte);
            continue;
        }
        out.push(b'#');
        push_hex(byte, out);
    }
}

/// Writes a string object. Byte content that is printable ASCII (plus tab,
/// newline and carriage return) uses the literal form with `\`-escapes;
/// anything else uses the hex form. The choice is a pure function of the
/// bytes, keeping output deterministic.
pub fn write_string(bytes: &[u8], out: &mut Vec<u8>) {
    let literal = bytes
        .iter()
        .all(|&b| (0x20..=0x7E).contains(&b) || matches!(b, b'\n' | b'\r' | b'\t'));
    if !literal {
        out.push(b'<');
        for &byte in bytes {
            push_hex(byte, out);
        }
        out.push(b'>');
        return;
    }
    out.push(b'(');
    for &byte in bytes {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(byte);
            }
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            other => out.push(other),
        }
    }
    out.push(b')');
}

/// Writes a real number in plain decimal: no exponent, trailing zeros
/// trimmed, `-0` normalized to `0`, non-finite values written as `0`.
/// Whole values write as integers (`72` not `72.0`).
pub fn write_real(value: f64, out: &mut Vec<u8>) {
    if !value.is_finite() || value == 0.0 {
        out.push(b'0');
        return;
    }
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        out.extend_from_slice((value as i64).to_string().as_bytes());
        return;
    }
    out.extend_from_slice(plain_decimal(&value.to_string()).as_bytes());
}

/// Like [`write_real`], for `f32` values (content-stream operands): the
/// shortest decimal that parses back to the identical `f32`.
pub fn write_real_f32(value: f32, out: &mut Vec<u8>) {
    if !value.is_finite() || value == 0.0 {
        out.push(b'0');
        return;
    }
    out.extend_from_slice(plain_decimal(&value.to_string()).as_bytes());
}

/// Rejects a name containing NUL: ISO 32000 forbids the `#00` escape, so
/// such a name has no legal spelling. Object and dictionary serialization
/// validate here; [`write_name`] itself stays infallible for callers whose
/// names are structurally NUL-free (content-stream resource names).
fn nul_free(name: &Name) -> Result<()> {
    if name.0.bytes().all(|b| b != 0) {
        return Ok(());
    }
    Err(Error::Other(format!(
        "name {:?} contains NUL, which has no legal escape in a name",
        name.0
    )))
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn push_hex(byte: u8, out: &mut Vec<u8>) {
    out.push(HEX[usize::from(byte >> 4)]);
    out.push(HEX[usize::from(byte & 0x0F)]);
}

fn plain_decimal(formatted: &str) -> String {
    let expanded = match formatted.split_once(['e', 'E']) {
        None => formatted.to_string(),
        Some((mantissa, exponent)) => expand_exponent(mantissa, exponent),
    };
    if !expanded.contains('.') {
        return expanded;
    }
    expanded
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn expand_exponent(mantissa: &str, exponent: &str) -> String {
    let exp: i32 = exponent
        .parse()
        .expect("float formatting produced a malformed exponent");
    let (sign, unsigned) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let (int_part, frac_part) = match unsigned.split_once('.') {
        Some(parts) => parts,
        None => (unsigned, ""),
    };
    let digits = format!("{int_part}{frac_part}");
    let point = int_part.len() as i32 + exp;
    if point <= 0 {
        let zeros = "0".repeat(point.unsigned_abs() as usize);
        return format!("{sign}0.{zeros}{digits}");
    }
    let point = point as usize;
    if point >= digits.len() {
        let zeros = "0".repeat(point - digits.len());
        return format!("{sign}{digits}{zeros}");
    }
    format!("{sign}{}.{}", &digits[..point], &digits[point..])
}

#[cfg(test)]
mod tests {
    use pdfboss_core::parser::{NoResolve, Parser};
    use pdfboss_core::{Dict, Name, ObjRef, Object, Stream};

    use super::*;
    use crate::error::Error;

    fn name(text: &str) -> Name {
        Name(text.to_string())
    }

    fn ser(obj: &Object) -> String {
        let mut out = Vec::new();
        serialize_object(obj, &mut out).expect("serializable object");
        String::from_utf8(out).expect("serialized syntax is UTF-8")
    }

    fn real_str(value: f64) -> String {
        let mut out = Vec::new();
        write_real(value, &mut out);
        String::from_utf8(out).expect("real syntax is ASCII")
    }

    fn real32_str(value: f32) -> String {
        let mut out = Vec::new();
        write_real_f32(value, &mut out);
        String::from_utf8(out).expect("real syntax is ASCII")
    }

    fn name_str(text: &str) -> String {
        let mut out = Vec::new();
        write_name(text, &mut out);
        String::from_utf8(out).expect("name syntax is ASCII")
    }

    fn string_str(bytes: &[u8]) -> String {
        let mut out = Vec::new();
        write_string(bytes, &mut out);
        String::from_utf8(out).expect("string syntax is ASCII")
    }

    #[test]
    fn scalars_serialize_exactly() {
        assert_eq!(ser(&Object::Null), "null");
        assert_eq!(ser(&Object::Bool(true)), "true");
        assert_eq!(ser(&Object::Bool(false)), "false");
        assert_eq!(ser(&Object::Int(42)), "42");
        assert_eq!(ser(&Object::Int(-7)), "-7");
        assert_eq!(ser(&Object::Int(i64::MIN)), "-9223372036854775808");
        assert_eq!(ser(&Object::Int(i64::MAX)), "9223372036854775807");
    }

    #[test]
    fn real_pins_exact_bytes() {
        assert_eq!(real_str(0.0), "0");
        assert_eq!(real_str(-0.0), "0");
        assert_eq!(real_str(f64::NAN), "0");
        assert_eq!(real_str(f64::INFINITY), "0");
        assert_eq!(real_str(f64::NEG_INFINITY), "0");
        assert_eq!(real_str(72.0), "72");
        assert_eq!(real_str(-72.0), "-72");
        assert_eq!(real_str(0.5), "0.5");
        assert_eq!(real_str(-0.25), "-0.25");
        assert_eq!(real_str(0.1), "0.1");
        assert_eq!(real_str(1e-7), "0.0000001");
        assert_eq!(real_str(1.5e-10), "0.00000000015");
        assert_eq!(real_str(9007199254740991.0), "9007199254740991");
        assert_eq!(real_str(-9007199254740991.0), "-9007199254740991");
        assert_eq!(real_str(9007199254740992.0), "9007199254740992");
        assert_eq!(real_str(1e300), format!("1{}", "0".repeat(300)));
    }

    #[test]
    fn real_f32_round_trips_hard_cases() {
        let cases = [
            0.1f32,
            1e-7f32,
            16777216.0f32,
            f32::MIN_POSITIVE,
            3.4e38f32,
            1073741824.0f32,
        ];
        for value in cases {
            let text = real32_str(value);
            assert!(
                !text.contains(['e', 'E']),
                "{value} produced exponent form {text}"
            );
            let parsed: f32 = text.parse().expect("output parses as f32");
            assert_eq!(parsed.to_bits(), value.to_bits(), "{value} -> {text}");
        }
        assert_eq!(real32_str(-0.0f32), "0");
        assert_eq!(real32_str(f32::NAN), "0");
        assert_eq!(real32_str(f32::INFINITY), "0");
    }

    #[test]
    fn real_f32_pins_exact_bytes() {
        assert_eq!(real32_str(0.1f32), "0.1");
        assert_eq!(real32_str(1e-7f32), "0.0000001");
        assert_eq!(real32_str(16777216.0f32), "16777216");
        assert_eq!(real32_str(1073741824.0f32), "1073741800");
        assert_eq!(real32_str(3.4e38f32), format!("34{}", "0".repeat(37)));
        assert_eq!(
            real32_str(f32::MIN_POSITIVE),
            format!("0.{}11754944", "0".repeat(37))
        );
    }

    #[test]
    fn plain_decimal_expands_exponents() {
        assert_eq!(plain_decimal("1e300"), format!("1{}", "0".repeat(300)));
        assert_eq!(plain_decimal("3.4e38"), format!("34{}", "0".repeat(37)));
        assert_eq!(
            plain_decimal("1.1754944e-38"),
            format!("0.{}11754944", "0".repeat(37))
        );
        assert_eq!(plain_decimal("-2.5e3"), "-2500");
        assert_eq!(plain_decimal("1.25e2"), "125");
        assert_eq!(plain_decimal("1.25e1"), "12.5");
        assert_eq!(plain_decimal("1.25e-1"), "0.125");
        assert_eq!(plain_decimal("1e-1"), "0.1");
        assert_eq!(plain_decimal("2.5E3"), "2500");
        assert_eq!(plain_decimal("0.5"), "0.5");
        assert_eq!(plain_decimal("1.50"), "1.5");
        assert_eq!(plain_decimal("2.0"), "2");
    }

    #[test]
    fn names_escape_irregular_bytes() {
        assert_eq!(name_str("Type"), "/Type");
        assert_eq!(name_str(""), "/");
        assert_eq!(name_str("A B"), "/A#20B");
        assert_eq!(name_str("A#B"), "/A#23B");
        assert_eq!(name_str("A(B)"), "/A#28B#29");
        assert_eq!(name_str("a/b"), "/a#2Fb");
        assert_eq!(name_str("x[y]z"), "/x#5By#5Dz");
        assert_eq!(name_str("{}"), "/#7B#7D");
        assert_eq!(name_str("<>"), "/#3C#3E");
        assert_eq!(name_str("%"), "/#25");
        assert_eq!(name_str("Ä"), "/#C3#84");
        assert_eq!(name_str("é"), "/#C3#A9");
        assert_eq!(name_str("\u{7F}"), "/#7F");
        assert_eq!(name_str("~!$&*+-.;=?@^"), "/~!$&*+-.;=?@^");
    }

    #[test]
    fn strings_use_literal_form_with_escapes() {
        assert_eq!(string_str(b"Hello"), "(Hello)");
        assert_eq!(string_str(b""), "()");
        assert_eq!(string_str(b"a(b)c"), "(a\\(b\\)c)");
        assert_eq!(string_str(b"a\\b"), "(a\\\\b)");
        assert_eq!(string_str(b"a\tb\nc\rd"), "(a\\tb\\nc\\rd)");
    }

    #[test]
    fn strings_fall_back_to_hex_form() {
        assert_eq!(string_str(&[0x00, 0xFF, 0x41]), "<00FF41>");
        assert_eq!(string_str(&[0x7F]), "<7F>");
        assert_eq!(string_str(&[0x1F]), "<1F>");
        let long: Vec<u8> = (0u8..=255).collect();
        let hex: String = (0u8..=255).map(|b| format!("{b:02X}")).collect();
        assert_eq!(string_str(&long), format!("<{hex}>"));
    }

    #[test]
    fn arrays_separate_items_with_single_spaces() {
        assert_eq!(ser(&Object::Array(vec![])), "[]");
        let arr = Object::Array(vec![
            Object::Int(1),
            Object::Real(2.5),
            Object::Name(name("X")),
        ]);
        assert_eq!(ser(&arr), "[1 2.5 /X]");
        let nested = Object::Array(vec![
            Object::Array(vec![Object::Int(1), Object::Int(2)]),
            Object::Array(vec![Object::Int(3)]),
        ]);
        assert_eq!(ser(&nested), "[[1 2] [3]]");
    }

    #[test]
    fn dicts_sort_keys_bytewise() {
        assert_eq!(ser(&Object::Dict(Dict::new())), "<< >>");

        let mut d = Dict::new();
        d.insert(name("Z"), Object::Int(2));
        d.insert(name("A"), Object::Int(1));
        assert_eq!(ser(&Object::Dict(d)), "<< /A 1 /Z 2 >>");

        let mut d = Dict::new();
        d.insert(name("a"), Object::Int(4));
        d.insert(name("B"), Object::Int(3));
        d.insert(name("AB"), Object::Int(2));
        d.insert(name("AA"), Object::Int(1));
        assert_eq!(ser(&Object::Dict(d)), "<< /AA 1 /AB 2 /B 3 /a 4 >>");
    }

    #[test]
    fn nested_containers_serialize_recursively() {
        let mut inner = Dict::new();
        inner.insert(name("X"), Object::Int(1));
        let mut outer = Dict::new();
        outer.insert(name("D"), Object::Dict(inner));
        outer.insert(
            name("Arr"),
            Object::Array(vec![Object::Null, Object::Bool(false)]),
        );
        assert_eq!(
            ser(&Object::Dict(outer)),
            "<< /Arr [null false] /D << /X 1 >> >>"
        );
    }

    #[test]
    fn refs_serialize_as_num_gen_r() {
        assert_eq!(ser(&Object::Ref(ObjRef { num: 12, gen: 3 })), "12 3 R");
        assert_eq!(ser(&Object::Ref(ObjRef { num: 1, gen: 0 })), "1 0 R");
    }

    #[test]
    fn nested_streams_are_errors() {
        let stream = Object::Stream(Stream {
            dict: Dict::new(),
            data: b"x".to_vec(),
        });
        let mut out = Vec::new();
        let top = serialize_object(&stream, &mut out);
        assert!(matches!(top, Err(Error::NestedStream)));

        let in_array = serialize_object(&Object::Array(vec![stream.clone()]), &mut out);
        assert!(matches!(in_array, Err(Error::NestedStream)));

        let mut d = Dict::new();
        d.insert(name("S"), stream);
        let in_dict = serialize_object(&Object::Dict(d), &mut out);
        assert!(matches!(in_dict, Err(Error::NestedStream)));
    }

    #[test]
    fn serialized_objects_parse_back_equal() {
        let mut inner = Dict::new();
        inner.insert(name("Z"), Object::Int(1));
        inner.insert(
            name("A"),
            Object::Array(vec![Object::Real(0.5), Object::Null]),
        );
        let mut outer = Dict::new();
        outer.insert(
            name("Kids"),
            Object::Array(vec![Object::Ref(ObjRef { num: 7, gen: 0 })]),
        );
        outer.insert(name("Inner"), Object::Dict(inner));
        outer.insert(name("Weird Name#"), Object::String(b"a(b)\\c".to_vec()));

        let objects = vec![
            Object::Null,
            Object::Bool(true),
            Object::Bool(false),
            Object::Int(-42),
            Object::Real(2.5),
            Object::Real(-0.125),
            Object::String(b"a(b)\\c".to_vec()),
            Object::String(b"tab\there".to_vec()),
            Object::String(vec![0u8, 255, 128]),
            Object::Name(name("Weird Name#\u{C4}")),
            Object::Ref(ObjRef { num: 12, gen: 3 }),
            Object::Array(vec![]),
            Object::Dict(Dict::new()),
            Object::Dict(outer),
        ];
        for obj in objects {
            let mut out = Vec::new();
            serialize_object(&obj, &mut out).expect("serializable object");
            let parsed = Parser::new(&out)
                .parse_object(&NoResolve)
                .expect("serialized bytes parse");
            assert_eq!(
                parsed,
                obj,
                "round-trip of {}",
                String::from_utf8_lossy(&out)
            );
        }
    }

    #[test]
    fn integral_reals_parse_back_as_ints() {
        let mut out = Vec::new();
        serialize_object(&Object::Real(72.0), &mut out).expect("serializable object");
        assert_eq!(out, b"72");
        let parsed = Parser::new(&out)
            .parse_object(&NoResolve)
            .expect("serialized bytes parse");
        assert_eq!(parsed, Object::Int(72));
    }

    #[test]
    fn names_with_nul_bytes_are_rejected() {
        let mut out = Vec::new();
        let err = serialize_object(&Object::Name(name("a\0b")), &mut out)
            .expect_err("a NUL in a name must not serialize");
        assert!(err.to_string().contains("NUL"), "{err}");
        let mut d = Dict::new();
        d.insert(name("a\0b"), Object::Int(1));
        let mut out = Vec::new();
        assert!(serialize_dict(&d, &mut out).is_err());
    }
}
