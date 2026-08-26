//! COS object serialization: `Object` values to PDF syntax bytes
//! (ISO 32000 §7.3). Output is deterministic — dictionary keys are emitted
//! in sorted order regardless of insertion order.
//!
//! Streams are deliberately absent here: a stream is only legal as an
//! indirect object and its `/Length` bookkeeping belongs to the
//! [`Writer`](crate::Writer), so a nested `Object::Stream` is an error.

use pdfboss_core::{Dict, Object};

use crate::error::Result;

/// Serializes any non-stream object into `out`.
///
/// Numbers are written without exponents; reals trim trailing zeros and
/// never produce `-0`. Non-finite reals serialize as `0` (PDF has no
/// representation for them).
pub fn serialize_object(obj: &Object, out: &mut Vec<u8>) -> Result<()> {
    let unused = (obj, out);
    todo!("serialize object: {unused:?}")
}

/// Serializes a dictionary with `<< … >>` delimiters, keys sorted
/// bytewise for deterministic output.
pub fn serialize_dict(dict: &Dict, out: &mut Vec<u8>) -> Result<()> {
    let unused = (dict, out);
    todo!("serialize dict: {unused:?}")
}

/// Writes a name object with its leading solidus, escaping every byte
/// outside the regular range as `#xx` (delimiters, whitespace, `#` itself,
/// and anything outside `0x21..=0x7E`).
pub fn write_name(name: &str, out: &mut Vec<u8>) {
    let unused = (name, out);
    todo!("write name: {unused:?}")
}

/// Writes a string object. Byte content that is printable ASCII (plus tab,
/// newline and carriage return) uses the literal form with `\`-escapes;
/// anything else uses the hex form. The choice is a pure function of the
/// bytes, keeping output deterministic.
pub fn write_string(bytes: &[u8], out: &mut Vec<u8>) {
    let unused = (bytes, out);
    todo!("write string: {unused:?}")
}

/// Writes a real number in plain decimal: no exponent, trailing zeros
/// trimmed, `-0` normalized to `0`, non-finite values written as `0`.
/// Whole values write as integers (`72` not `72.0`).
pub fn write_real(value: f64, out: &mut Vec<u8>) {
    let unused = (value, out);
    todo!("write real: {unused:?}")
}

/// Like [`write_real`], for `f32` values (content-stream operands): the
/// shortest decimal that parses back to the identical `f32`.
pub fn write_real_f32(value: f32, out: &mut Vec<u8>) {
    let unused = (value, out);
    todo!("write f32: {unused:?}")
}
