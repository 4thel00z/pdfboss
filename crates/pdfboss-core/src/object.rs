//! The PDF object model: the nine basic object types plus indirect
//! references (ISO 32000 §7.3).

use std::borrow::Borrow;

use crate::hash::FastMap;

/// An indirect object reference: object number and generation number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjRef {
    pub num: u32,
    pub gen: u16,
}

/// A name object, stored decoded (any `#xx` escapes already resolved) and
/// without the leading solidus.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(pub String);

impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// A dictionary object mapping names to objects.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dict(FastMap<Name, Object>);

impl Dict {
    /// Creates an empty dictionary.
    pub fn new() -> Dict {
        Dict::default()
    }

    /// Looks up a value by key (without the leading solidus).
    ///
    /// Covers ISO 32000-1 §7.3.7.
    pub fn get(&self, key: &str) -> Option<&Object> {
        self.0.get(key)
    }

    /// Inserts a key/value pair, returning the previous value if any.
    pub fn insert(&mut self, key: Name, value: Object) -> Option<Object> {
        self.0.insert(key, value)
    }

    /// Removes a key, returning its value if present.
    pub fn remove(&mut self, key: &str) -> Option<Object> {
        self.0.remove(key)
    }

    /// Iterates over all key/value pairs (unordered).
    pub fn iter(&self) -> impl Iterator<Item = (&Name, &Object)> {
        self.0.iter()
    }

    /// Mutable iteration over all values (used by in-place decryption).
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut Object> {
        self.0.values_mut()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the dictionary has no entries.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Typed lookup: integer value.
    pub fn get_int(&self, key: &str) -> Option<i64> {
        self.get(key)?.as_int()
    }

    /// Typed lookup: numeric value as `f64` (integers coerce).
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key)?.as_f64()
    }

    /// Typed lookup: name value.
    pub fn get_name(&self, key: &str) -> Option<&Name> {
        self.get(key)?.as_name()
    }

    /// Typed lookup: array value.
    pub fn get_array(&self, key: &str) -> Option<&[Object]> {
        self.get(key)?.as_array()
    }

    /// Typed lookup: dictionary value.
    pub fn get_dict(&self, key: &str) -> Option<&Dict> {
        self.get(key)?.as_dict()
    }

    /// Typed lookup: indirect reference value.
    pub fn get_ref(&self, key: &str) -> Option<ObjRef> {
        self.get(key)?.as_ref()
    }
}

/// A stream object: its dictionary plus the raw, still-encoded data bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    /// Raw stream bytes as stored in the file (filters not yet applied).
    pub data: Vec<u8>,
}

/// Any PDF object (ISO 32000 §7.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    /// A string object as raw bytes (may be text or binary data).
    String(Vec<u8>),
    Name(Name),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Stream),
    Ref(ObjRef),
}

impl Object {
    /// Boolean value, if this is a `Bool`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Object::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Integer value, if this is an `Int`.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Object::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Numeric value as `f64`; `Int` coerces.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Object::Int(i) => Some(*i as f64),
            Object::Real(r) => Some(*r),
            _ => None,
        }
    }

    /// Raw string bytes, if this is a `String`.
    ///
    /// Covers ISO 32000-1 §7.9.2 and §7.9.2.4.
    pub fn as_str_bytes(&self) -> Option<&[u8]> {
        match self {
            Object::String(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// Name value, if this is a `Name`.
    pub fn as_name(&self) -> Option<&Name> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    /// Array contents, if this is an `Array`.
    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Dictionary, if this is a `Dict` (or the dictionary of a `Stream`).
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    /// Stream, if this is a `Stream`.
    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Object::Stream(s) => Some(s),
            _ => None,
        }
    }

    /// Indirect reference, if this is a `Ref`.
    #[allow(clippy::should_implement_trait)] // accessor family named per spec
    pub fn as_ref(&self) -> Option<ObjRef> {
        match self {
            Object::Ref(r) => Some(*r),
            _ => None,
        }
    }

    /// Whether this object is `Null`.
    ///
    /// Covers ISO 32000-1 §7.3.9.
    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
    }
}

/// The character a PDFDocEncoding byte stands for, or `None` for the codes
/// the encoding leaves undefined (0x00 to 0x17 except tab, line feed and
/// carriage return, 0x7F, 0x9F and 0xAD). The encoding agrees with Latin-1
/// except for the eight accents at 0x18 to 0x1F, the punctuation and
/// ligatures at 0x80 to 0x9E and the Euro sign at 0xA0.
///
/// Covers ISO 32000-1 §7.9.2.3 and Annex D.3.
pub fn pdf_doc_char(code: u8) -> Option<char> {
    let c = match code {
        0x09 | 0x0A | 0x0D | 0x20..=0x7E | 0xA1..=0xAC | 0xAE..=0xFF => char::from(code),
        0x18 => '\u{2D8}',
        0x19 => '\u{2C7}',
        0x1A => '\u{2C6}',
        0x1B => '\u{2D9}',
        0x1C => '\u{2DD}',
        0x1D => '\u{2DB}',
        0x1E => '\u{2DA}',
        0x1F => '\u{2DC}',
        0x80 => '\u{2022}',
        0x81 => '\u{2020}',
        0x82 => '\u{2021}',
        0x83 => '\u{2026}',
        0x84 => '\u{2014}',
        0x85 => '\u{2013}',
        0x86 => '\u{192}',
        0x87 => '\u{2044}',
        0x88 => '\u{2039}',
        0x89 => '\u{203A}',
        0x8A => '\u{2212}',
        0x8B => '\u{2030}',
        0x8C => '\u{201E}',
        0x8D => '\u{201C}',
        0x8E => '\u{201D}',
        0x8F => '\u{2018}',
        0x90 => '\u{2019}',
        0x91 => '\u{201A}',
        0x92 => '\u{2122}',
        0x93 => '\u{FB01}',
        0x94 => '\u{FB02}',
        0x95 => '\u{141}',
        0x96 => '\u{152}',
        0x97 => '\u{160}',
        0x98 => '\u{178}',
        0x99 => '\u{17D}',
        0x9A => '\u{131}',
        0x9B => '\u{142}',
        0x9C => '\u{153}',
        0x9D => '\u{161}',
        0x9E => '\u{17E}',
        0xA0 => '\u{20AC}',
        _ => return None,
    };
    Some(c)
}

/// Decodes a PDF text string: UTF-16BE with BOM, UTF-8 with BOM (PDF 2.0),
/// otherwise PDFDocEncoding, with the replacement character for the codes
/// that encoding leaves undefined.
///
/// Covers ISO 32000-1 §7.9.2, §7.9.2.2, §7.9.2.3 and Annex D.3.
pub fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
        let mut out: String = std::char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        if rest.len() % 2 == 1 {
            // A dangling trailing byte cannot form a UTF-16 code unit.
            out.push(char::REPLACEMENT_CHARACTER);
        }
        out
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes
            .iter()
            .map(|&b| pdf_doc_char(b).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> Name {
        Name(s.to_string())
    }

    #[test]
    fn accessors_return_some_for_matching_variant() {
        assert_eq!(Object::Bool(true).as_bool(), Some(true));
        assert_eq!(Object::Bool(false).as_bool(), Some(false));
        assert_eq!(Object::Int(-7).as_int(), Some(-7));
        assert_eq!(Object::Real(1.5).as_f64(), Some(1.5));
        assert_eq!(
            Object::String(b"abc".to_vec()).as_str_bytes(),
            Some(b"abc".as_slice())
        );
        assert_eq!(Object::Name(name("Type")).as_name(), Some(&name("Type")));
        let arr = Object::Array(vec![Object::Int(1), Object::Null]);
        assert_eq!(arr.as_array().map(<[Object]>::len), Some(2));
        let mut d = Dict::new();
        d.insert(name("K"), Object::Int(9));
        assert_eq!(Object::Dict(d.clone()).as_dict(), Some(&d));
        let r = ObjRef { num: 12, gen: 3 };
        assert_eq!(Object::Ref(r).as_ref(), Some(r));
        assert!(Object::Null.is_null());
        assert!(!Object::Int(0).is_null());
    }

    #[test]
    fn accessors_return_none_for_other_variants() {
        let o = Object::Int(1);
        assert_eq!(o.as_bool(), None);
        assert_eq!(o.as_str_bytes(), None);
        assert_eq!(o.as_name(), None);
        assert_eq!(o.as_array(), None);
        assert_eq!(o.as_dict(), None);
        assert_eq!(o.as_stream(), None);
        assert_eq!(o.as_ref(), None);
        assert_eq!(Object::Real(2.0).as_int(), None);
        assert_eq!(Object::Bool(true).as_f64(), None);
        assert_eq!(Object::Null.as_f64(), None);
    }

    #[test]
    fn as_f64_coerces_int() {
        assert_eq!(Object::Int(42).as_f64(), Some(42.0));
        assert_eq!(Object::Int(-3).as_f64(), Some(-3.0));
        assert_eq!(Object::Real(0.25).as_f64(), Some(0.25));
    }

    #[test]
    fn as_dict_sees_stream_dict() {
        let mut d = Dict::new();
        d.insert(name("Length"), Object::Int(5));
        let s = Stream {
            dict: d.clone(),
            data: b"hello".to_vec(),
        };
        let o = Object::Stream(s.clone());
        assert_eq!(o.as_dict(), Some(&d));
        assert_eq!(o.as_stream(), Some(&s));
    }

    #[test]
    fn dict_basic_operations() {
        let mut d = Dict::new();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
        assert_eq!(d.insert(name("A"), Object::Int(1)), None);
        assert_eq!(
            d.insert(name("A"), Object::Int(2)),
            Some(Object::Int(1)),
            "insert returns the previous value"
        );
        d.insert(name("B"), Object::Bool(true));
        assert_eq!(d.len(), 2);
        assert!(!d.is_empty());
        assert_eq!(d.get("A"), Some(&Object::Int(2)));
        assert_eq!(d.get("Missing"), None);
        let mut keys: Vec<&str> = d.iter().map(|(k, _)| k.0.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["A", "B"]);
        assert_eq!(d.remove("A"), Some(Object::Int(2)));
        assert_eq!(d.remove("A"), None);
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn dict_typed_helpers() {
        let mut inner = Dict::new();
        inner.insert(name("X"), Object::Null);
        let mut d = Dict::new();
        d.insert(name("Int"), Object::Int(7));
        d.insert(name("Real"), Object::Real(2.5));
        d.insert(name("Name"), Object::Name(name("Page")));
        d.insert(name("Arr"), Object::Array(vec![Object::Int(1)]));
        d.insert(name("Dict"), Object::Dict(inner.clone()));
        d.insert(name("Ref"), Object::Ref(ObjRef { num: 4, gen: 0 }));

        assert_eq!(d.get_int("Int"), Some(7));
        assert_eq!(d.get_int("Real"), None, "reals do not coerce to int");
        assert_eq!(d.get_f64("Int"), Some(7.0), "ints coerce to f64");
        assert_eq!(d.get_f64("Real"), Some(2.5));
        assert_eq!(d.get_name("Name"), Some(&name("Page")));
        assert_eq!(d.get_array("Arr"), Some([Object::Int(1)].as_slice()));
        assert_eq!(d.get_dict("Dict"), Some(&inner));
        assert_eq!(d.get_ref("Ref"), Some(ObjRef { num: 4, gen: 0 }));

        // Missing key and wrong type both yield None.
        assert_eq!(d.get_int("Nope"), None);
        assert_eq!(d.get_name("Int"), None);
        assert_eq!(d.get_array("Dict"), None);
        assert_eq!(d.get_dict("Arr"), None);
        assert_eq!(d.get_ref("Int"), None);
    }

    // Covers ISO 32000-1 §7.9.2 and §7.9.2.2.
    #[test]
    fn decode_utf16be_with_bom() {
        assert_eq!(
            decode_text_string(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]),
            "Hi"
        );
        // Surrogate pair: U+1D11E.
        assert_eq!(
            decode_text_string(&[0xFE, 0xFF, 0xD8, 0x34, 0xDD, 0x1E]),
            "\u{1D11E}"
        );
        // BOM only decodes to the empty string.
        assert_eq!(decode_text_string(&[0xFE, 0xFF]), "");
        // A lone surrogate becomes U+FFFD.
        assert_eq!(decode_text_string(&[0xFE, 0xFF, 0xD8, 0x34]), "\u{FFFD}");
        // An odd trailing byte becomes U+FFFD.
        assert_eq!(
            decode_text_string(&[0xFE, 0xFF, 0x00, 0x48, 0x12]),
            "H\u{FFFD}"
        );
    }

    // Covers ISO 32000-1 §7.9.2.2.
    #[test]
    fn decode_utf8_with_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("héllo €".as_bytes());
        assert_eq!(decode_text_string(&bytes), "héllo €");
        assert_eq!(decode_text_string(&[0xEF, 0xBB, 0xBF]), "");
        // Invalid UTF-8 after the BOM is replaced, not an error.
        assert_eq!(decode_text_string(&[0xEF, 0xBB, 0xBF, 0xFF]), "\u{FFFD}");
    }

    // Covers ISO 32000-1 §7.9.2, §7.9.2.3 and Annex D.3.
    #[test]
    fn decode_without_bom_uses_pdf_doc_encoding() {
        assert_eq!(decode_text_string(b"Hello"), "Hello");
        assert_eq!(decode_text_string(&[0x48, 0xE9]), "Hé");
        assert_eq!(decode_text_string(&[0xFF]), "ÿ");
        assert_eq!(decode_text_string(&[]), "");
        // A lone 0xFE (no full UTF-16 BOM) is PDFDocEncoding's thorn.
        assert_eq!(decode_text_string(&[0xFE]), "\u{FE}");
    }

    // Covers ISO 32000-1 §7.9.2.3 and Annex D.3: the codes where
    // PDFDocEncoding departs from Latin-1.
    #[test]
    fn decode_pdf_doc_encoding_accents_at_0x18() {
        assert_eq!(
            decode_text_string(&[0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F]),
            "\u{2D8}\u{2C7}\u{2C6}\u{2D9}\u{2DD}\u{2DB}\u{2DA}\u{2DC}"
        );
    }

    // Covers ISO 32000-1 §7.9.2.3 and Annex D.3.
    #[test]
    fn decode_pdf_doc_encoding_punctuation_at_0x80() {
        assert_eq!(
            decode_text_string(&[0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87]),
            "•†‡…—–ƒ⁄"
        );
        assert_eq!(
            decode_text_string(&[0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F]),
            "‹›−‰„“”‘"
        );
        assert_eq!(
            decode_text_string(&[0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97]),
            "’‚™ﬁﬂŁŒŠ"
        );
        assert_eq!(
            decode_text_string(&[0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E]),
            "ŸŽıłœšž"
        );
        assert_eq!(decode_text_string(&[0xA0]), "€");
    }

    // Covers ISO 32000-1 §7.9.2.3 and Annex D.3: everywhere else the two
    // agree, so Latin-1 text keeps decoding as before.
    #[test]
    fn decode_pdf_doc_encoding_agrees_with_latin1_elsewhere() {
        assert_eq!(
            decode_text_string(&[0x09, 0x0A, 0x0D, 0x20, 0x7E, 0xA1, 0xE9, 0xFE, 0xFF]),
            "\t\n\r ~¡éþÿ"
        );
    }

    // Covers ISO 32000-1 §7.9.2.3 and Annex D.3: codes the encoding leaves
    // undefined decode to the replacement character instead of a guess.
    #[test]
    fn decode_pdf_doc_encoding_undefined_codes_are_replaced() {
        assert_eq!(decode_text_string(&[0x9F, 0xAD]), "\u{FFFD}\u{FFFD}");
        assert_eq!(decode_text_string(&[0x00, 0x7F]), "\u{FFFD}\u{FFFD}");
    }
}
