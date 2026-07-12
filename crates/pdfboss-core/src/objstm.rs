//! Object streams (ISO 32000 §7.5.7): compressed objects stored inside a
//! `/Type/ObjStm` stream.

use crate::error::{Error, Result};
use crate::lexer::{Lexer, Token};
use crate::object::Object;
use crate::parser::{NoResolve, Parser};

/// Extracts the object at `index` from decoded object-stream data.
///
/// The header consists of `2*n` integers — pairs of object number and byte
/// offset — followed by the object bodies; offsets are relative to `first`,
/// the position where the first body begins.
pub fn extract(stream_data: &[u8], n: usize, first: usize, index: u32) -> Result<Object> {
    let idx = index as usize;
    if idx >= n {
        return Err(Error::Other(format!(
            "object stream index {index} out of range (N = {n})"
        )));
    }
    let mut lexer = Lexer::new(stream_data);
    for _ in 0..idx {
        expect_int(&mut lexer)?; // object number
        expect_int(&mut lexer)?; // offset
    }
    expect_int(&mut lexer)?; // object number of the wanted entry
    let offset = expect_int(&mut lexer)?;
    let pos = first
        .checked_add(offset)
        .filter(|&p| p <= stream_data.len())
        .ok_or_else(|| {
            Error::Other(format!(
                "object stream offset {offset} lies outside the stream"
            ))
        })?;
    Parser::at(stream_data, pos).parse_object(&NoResolve)
}

/// Reads one non-negative integer from the object-stream header.
fn expect_int(lexer: &mut Lexer) -> Result<usize> {
    match lexer.next_token()? {
        Token::Int(v) if v >= 0 => Ok(v as usize),
        _ => Err(Error::Syntax {
            offset: lexer.pos(),
            msg: "malformed object stream header".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds decoded object-stream bytes for `(num, body)` pairs; returns
    /// `(data, n, first)`.
    fn build_stream(objects: &[(u32, &str)]) -> (Vec<u8>, usize, usize) {
        let mut header = String::new();
        let mut bodies = String::new();
        for (num, body) in objects {
            header.push_str(&format!("{} {} ", num, bodies.len()));
            bodies.push_str(body);
            bodies.push(' ');
        }
        let first = header.len();
        header.push_str(&bodies);
        (header.into_bytes(), objects.len(), first)
    }

    #[test]
    fn extracts_both_objects() {
        let (data, n, first) = build_stream(&[(11, "<< /A 1 >>"), (12, "(hi)")]);
        let obj0 = extract(&data, n, first, 0).unwrap();
        assert_eq!(obj0.as_dict().unwrap().get_int("A"), Some(1));
        let obj1 = extract(&data, n, first, 1).unwrap();
        assert_eq!(obj1, Object::String(b"hi".to_vec()));
    }

    #[test]
    fn extracts_from_testkit_payload() {
        let (_, payload) = pdfboss_testkit::objstm_payload(&[(3, "[1 2 3]"), (9, "/Name")]);
        // Header of 2*n integers; /First is where the first body starts.
        let n = 2;
        let first = payload.iter().position(|&b| b == b'\n').unwrap() + 1;
        let array = extract(&payload, n, first, 0).unwrap();
        assert_eq!(array.as_array().unwrap().len(), 3);
        let name = extract(&payload, n, first, 1).unwrap();
        assert_eq!(name.as_name().map(|nm| nm.0.as_str()), Some("Name"));
    }

    #[test]
    fn index_out_of_range_errors() {
        let (data, n, first) = build_stream(&[(1, "true"), (2, "null")]);
        assert!(extract(&data, n, first, 2).is_err());
        assert!(extract(&data, n, first, u32::MAX).is_err());
    }

    #[test]
    fn malformed_header_errors() {
        assert!(extract(b"/NotAnInt 0 true", 1, 12, 0).is_err());
        assert!(extract(b"5 -3 true", 1, 5, 0).is_err());
    }

    #[test]
    fn offset_beyond_data_errors() {
        assert!(extract(b"1 999\ntrue", 1, 6, 0).is_err());
    }
}
