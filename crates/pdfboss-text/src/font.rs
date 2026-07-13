//! Font loading from a page's `/Font` resource dictionary: simple fonts
//! (byte codes, `/Encoding` + `/Differences`, `/Widths`) and Type0/CID
//! fonts (2-byte codes, `/ToUnicode`, descendant `/W` + `/DW`).

use crate::cmap::ToUnicode;
use crate::encodings;
use pdfboss_core::{Dict, Document, Object};
use std::collections::HashMap;

/// A loaded font: everything needed to decode show-string bytes to
/// Unicode and to advance the text position.
pub struct Font {
    /// True for simple (1-byte-code) fonts; false for Type0/CID fonts,
    /// whose codes are two bytes.
    pub simple: bool,
    /// `/ToUnicode` CMap when present — the highest-priority mapping.
    to_unicode: Option<ToUnicode>,
    /// Per-code Unicode from the `/Encoding` base table plus
    /// `/Differences` (simple fonts only).
    encoding: Option<Box<[Option<char>; 256]>>,
    /// Explicit widths per code, in glyph-space units (1/1000 em).
    widths: HashMap<u32, f32>,
    /// Width used for codes without an explicit entry.
    default_width: f32,
    /// The code that triggers word spacing (single-byte code 32).
    space_code: Option<u32>,
}

/// Resolves `dict[key]`, treating resolution failures and `null` as absent.
fn rv(doc: &Document, dict: &Dict, key: &str) -> Option<Object> {
    let obj = dict.get(key)?;
    let resolved = doc.resolve(obj).ok()?;
    (!resolved.is_null()).then_some(resolved)
}

impl Font {
    /// A last-resort font for missing or unloadable font resources:
    /// 1-byte codes, StandardEncoding fallback, width 500.
    pub fn fallback() -> Font {
        Font {
            simple: true,
            to_unicode: None,
            encoding: None,
            widths: HashMap::new(),
            default_width: 500.0,
            space_code: Some(32),
        }
    }

    /// Loads a font from its (resolved) font dictionary. Lenient: anything
    /// missing or malformed degrades to defaults rather than failing.
    pub fn load(doc: &Document, dict: &Dict) -> Font {
        let is_type0 = rv(doc, dict, "Subtype")
            .and_then(|o| o.as_name().map(|n| n.0.clone()))
            .is_some_and(|n| n == "Type0");
        let to_unicode = rv(doc, dict, "ToUnicode")
            .and_then(|o| o.as_stream().and_then(|s| doc.stream_data(s).ok()))
            .map(|data| ToUnicode::parse(&data))
            .filter(|c| !c.is_empty());
        if is_type0 {
            Font::load_type0(doc, dict, to_unicode)
        } else {
            Font::load_simple(doc, dict, to_unicode)
        }
    }

    /// Splits a show-string into character codes (1 or 2 bytes each).
    /// A trailing odd byte of a 2-byte font becomes its own code.
    pub fn codes(&self, bytes: &[u8]) -> Vec<u32> {
        if self.simple {
            bytes.iter().map(|&b| u32::from(b)).collect()
        } else {
            bytes
                .chunks(2)
                .map(|c| {
                    if c.len() == 2 {
                        u32::from(u16::from_be_bytes([c[0], c[1]]))
                    } else {
                        u32::from(c[0])
                    }
                })
                .collect()
        }
    }

    /// Decodes one code to Unicode as a fresh `String` (test helper; lib code
    /// uses [`Font::decode_into`] to avoid the per-glyph allocation).
    #[cfg(test)]
    pub fn decode(&self, code: u32) -> String {
        let mut out = String::new();
        self.decode_into(code, &mut out);
        out
    }

    /// Decodes one code to Unicode, appending to `out`. Priority:
    /// `/ToUnicode`, then the `/Encoding`-derived table, then StandardEncoding
    /// (simple fonts), then U+FFFD. The common single-glyph paths push one
    /// `char` with no allocation; only a multi-unit `/ToUnicode` mapping copies
    /// a string.
    pub fn decode_into(&self, code: u32, out: &mut String) {
        if let Some(c) = self.to_unicode.as_ref() {
            if let Some(s) = c.lookup(code) {
                out.push_str(&s);
                return;
            }
        }
        if self.simple {
            if let Ok(byte) = u8::try_from(code) {
                if let Some(Some(c)) = self.encoding.as_ref().map(|t| t[byte as usize]) {
                    out.push(c);
                    return;
                }
                if let Some(c) = encodings::standard(byte) {
                    out.push(c);
                    return;
                }
            }
        }
        out.push('\u{FFFD}');
    }

    /// Glyph-space width (1/1000 em) of `code`.
    pub fn width(&self, code: u32) -> f32 {
        self.widths
            .get(&code)
            .copied()
            .unwrap_or(self.default_width)
    }

    /// True when showing `code` applies word spacing (`Tw`).
    pub fn is_space(&self, code: u32) -> bool {
        self.space_code == Some(code)
    }

    /// Loads a Type1/TrueType/Type3 font: 1-byte codes, `/Encoding` base
    /// plus `/Differences`, widths from `/FirstChar` + `/Widths`.
    fn load_simple(doc: &Document, dict: &Dict, to_unicode: Option<ToUnicode>) -> Font {
        let encoding = Font::load_encoding(doc, dict);

        let mut widths = HashMap::new();
        let first = rv(doc, dict, "FirstChar")
            .and_then(|o| o.as_int())
            .unwrap_or(0)
            .max(0) as u32;
        if let Some(Object::Array(items)) = rv(doc, dict, "Widths") {
            for (i, item) in items.iter().enumerate() {
                let Some(code) = first.checked_add(i as u32) else {
                    break; // /FirstChar so large the codes overflow u32
                };
                if let Some(w) = doc.resolve(item).ok().and_then(|o| o.as_f64()) {
                    widths.insert(code, w as f32);
                }
            }
        }
        let default_width = rv(doc, dict, "FontDescriptor")
            .and_then(|o| {
                let fd = o.as_dict()?.clone();
                rv(doc, &fd, "MissingWidth")?.as_f64()
            })
            .map(|w| w as f32)
            .unwrap_or(500.0);

        Font {
            simple: true,
            to_unicode,
            encoding,
            widths,
            default_width,
            space_code: Some(32),
        }
    }

    /// Builds the 256-entry Unicode table from `/Encoding`: a base table
    /// (named directly or via `/BaseEncoding`, default Standard) with
    /// `/Differences` glyph names applied on top.
    fn load_encoding(doc: &Document, dict: &Dict) -> Option<Box<[Option<char>; 256]>> {
        let enc = rv(doc, dict, "Encoding")?;
        let base_name = match &enc {
            Object::Name(n) => Some(n.0.clone()),
            Object::Dict(d) => {
                rv(doc, d, "BaseEncoding").and_then(|o| o.as_name().map(|n| n.0.clone()))
            }
            _ => None,
        };
        let base: fn(u8) -> Option<char> = match base_name.as_deref() {
            Some("WinAnsiEncoding") => encodings::win_ansi,
            Some("MacRomanEncoding") => encodings::mac_roman,
            _ => encodings::standard,
        };
        let mut table = Box::new([None; 256]);
        for (code, slot) in table.iter_mut().enumerate() {
            *slot = base(code as u8);
        }
        if let Object::Dict(d) = &enc {
            if let Some(Object::Array(diffs)) = rv(doc, d, "Differences") {
                let mut code: u32 = 0;
                for item in &diffs {
                    match doc.resolve(item).ok() {
                        Some(Object::Int(n)) => code = n.max(0) as u32,
                        Some(Object::Real(n)) => code = n.max(0.0) as u32,
                        Some(Object::Name(name)) => {
                            if code < 256 {
                                table[code as usize] = encodings::glyph_to_unicode(&name.0);
                            }
                            code = code.saturating_add(1);
                        }
                        _ => {}
                    }
                }
            }
        }
        Some(table)
    }

    /// Loads a Type0/CID font: 2-byte codes (Identity or `-H`/`-V` CMap
    /// names; any other encoding CMap is treated as 2-byte too), Unicode
    /// via `/ToUnicode` only, widths from the descendant's `/W` + `/DW`.
    fn load_type0(doc: &Document, dict: &Dict, to_unicode: Option<ToUnicode>) -> Font {
        let descendant = rv(doc, dict, "DescendantFonts")
            .and_then(|o| {
                let arr = o.as_array()?.to_vec();
                doc.resolve(arr.first()?).ok()
            })
            .and_then(|o| o.as_dict().cloned());

        let mut widths = HashMap::new();
        let mut default_width = 1000.0;
        if let Some(desc) = &descendant {
            if let Some(dw) = rv(doc, desc, "DW").and_then(|o| o.as_f64()) {
                default_width = dw as f32;
            }
            if let Some(Object::Array(w)) = rv(doc, desc, "W") {
                Font::parse_cid_widths(doc, &w, &mut widths);
            }
        }

        Font {
            simple: false,
            to_unicode,
            encoding: None,
            widths,
            default_width,
            space_code: None,
        }
    }

    /// Parses a CID `/W` array: `c [w1 w2 …]` gives consecutive widths
    /// from CID `c`; `c1 c2 w` gives every CID in `c1..=c2` width `w`
    /// (ranges capped at 65536 entries).
    fn parse_cid_widths(doc: &Document, items: &[Object], widths: &mut HashMap<u32, f32>) {
        let resolved: Vec<Object> = items
            .iter()
            .map(|o| doc.resolve(o).unwrap_or(Object::Null))
            .collect();
        let mut i = 0;
        while i < resolved.len() {
            let Some(first) = resolved[i].as_int() else {
                i += 1;
                continue;
            };
            let first = first.max(0) as u32;
            match resolved.get(i + 1) {
                Some(Object::Array(list)) => {
                    for (j, item) in list.iter().enumerate() {
                        let Some(cid) = first.checked_add(j as u32) else {
                            break; // start CID so large the CIDs overflow u32
                        };
                        if let Some(w) = doc.resolve(item).ok().and_then(|o| o.as_f64()) {
                            widths.insert(cid, w as f32);
                        }
                    }
                    i += 2;
                }
                Some(other) if other.as_f64().is_some() => {
                    let last = other.as_int().unwrap_or(first as i64).max(0) as u32;
                    let w = resolved.get(i + 2).and_then(|o| o.as_f64());
                    if let Some(w) = w {
                        let end = last.min(first.saturating_add(65535));
                        for c in first..=end.max(first) {
                            widths.insert(c, w as f32);
                        }
                    }
                    i += 3;
                }
                _ => i += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::ObjRef;
    use pdfboss_testkit::PdfBuilder;

    /// Builds a document whose object 5 is `font_body`; `extra` objects
    /// (e.g. ToUnicode streams) can reference or be referenced by it.
    fn font_from(font_body: &str, extra: &[(u32, &[u8])]) -> Font {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.object(5, font_body);
        for &(num, data) in extra {
            b.stream(num, "", data);
        }
        let doc = Document::load(b.build(1)).unwrap();
        let obj = doc.get(ObjRef { num: 5, gen: 0 }).unwrap();
        Font::load(&doc, obj.as_dict().unwrap())
    }

    #[test]
    fn simple_winansi_font() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
            &[],
        );
        assert!(f.simple);
        assert_eq!(f.decode(65), "A");
        assert_eq!(f.decode(0x93), "\u{201C}");
        assert_eq!(f.width(65), 500.0);
        assert!(f.is_space(32));
        assert!(!f.is_space(65));
        assert_eq!(f.codes(b"AB"), vec![65, 66]);
    }

    #[test]
    fn differences_and_widths() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Custom \
             /Encoding << /BaseEncoding /WinAnsiEncoding \
             /Differences [65 /alpha /uni0042] >> \
             /FirstChar 65 /Widths [600 700] >>",
            &[],
        );
        assert_eq!(f.decode(65), "\u{3B1}"); // /alpha
        assert_eq!(f.decode(66), "B"); // /uni0042
        assert_eq!(f.decode(67), "C"); // untouched base
        assert_eq!(f.width(65), 600.0);
        assert_eq!(f.width(66), 700.0);
        assert_eq!(f.width(67), 500.0); // default
    }

    #[test]
    fn missing_width_from_descriptor() {
        let f = font_from(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X \
             /FontDescriptor << /Type /FontDescriptor /MissingWidth 300 >> >>",
            &[],
        );
        assert_eq!(f.width(65), 300.0);
    }

    #[test]
    fn tounicode_beats_encoding() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /X \
             /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>",
            &[(6, b"1 beginbfchar <41> <0058> endbfchar")],
        );
        assert_eq!(f.decode(0x41), "X"); // ToUnicode wins over WinAnsi 'A'
        assert_eq!(f.decode(0x42), "B"); // falls through to WinAnsi
    }

    #[test]
    fn type0_font() {
        let cmap: &[u8] = b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
                            1 beginbfchar <0001> <03A9> endbfchar";
        let f = font_from(
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 \
             /DW 800 /W [1 [500 600] 10 12 250] >>] /ToUnicode 6 0 R >>",
            &[(6, cmap)],
        );
        assert!(!f.simple);
        assert_eq!(f.codes(b"\x00\x01\x00\x02"), vec![1, 2]);
        assert_eq!(f.codes(b"\x00\x01\x07"), vec![1, 7]); // odd tail
        assert_eq!(f.decode(1), "\u{3A9}");
        assert_eq!(f.decode(2), "\u{FFFD}"); // ToUnicode only for Type0
        assert_eq!(f.width(1), 500.0);
        assert_eq!(f.width(2), 600.0);
        assert_eq!(f.width(10), 250.0);
        assert_eq!(f.width(12), 250.0);
        assert_eq!(f.width(99), 800.0); // /DW
        assert!(!f.is_space(32));
    }

    #[test]
    fn huge_first_char_widths_do_not_overflow() {
        // /FirstChar u32::MAX: the second /Widths entry would overflow.
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /FirstChar 4294967295 /Widths [600 700] >>",
            &[],
        );
        assert_eq!(f.width(u32::MAX), 600.0);
        assert_eq!(f.width(65), 500.0); // overflowed entry dropped
    }

    #[test]
    fn differences_start_at_u32_max_does_not_overflow() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding << /Differences [4294967295 /a /b] >> >>",
            &[],
        );
        assert_eq!(f.decode(65), "A"); // base table untouched, no panic
                                       // Same start code reached via a Real that saturates to u32::MAX.
        let g = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding << /Differences [5000000000.0 /a /b] >> >>",
            &[],
        );
        assert_eq!(g.decode(65), "A");
    }

    #[test]
    fn huge_cid_w_list_start_does_not_overflow() {
        // List form of /W with start CID u32::MAX: the second width
        // entry would overflow.
        let f = font_from(
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 \
             /DW 800 /W [4294967295 [10 20]] >>] >>",
            &[],
        );
        assert_eq!(f.width(u32::MAX), 10.0);
        assert_eq!(f.width(0), 800.0); // overflowed entry dropped -> /DW
    }

    #[test]
    fn fallback_font_uses_standard() {
        let f = Font::fallback();
        assert_eq!(f.decode(65), "A");
        assert_eq!(f.decode(0xA9), "\u{27}"); // Standard quotesingle
        assert_eq!(f.decode(0), "\u{FFFD}");
        assert_eq!(f.width(65), 500.0);
    }
}
