//! Bridges a `/Type3` font dictionary (ISO 32000-1 §9.6.5) to the pieces its
//! executor needs to paint glyphs: the glyph-space-to-text-space
//! `/FontMatrix`, the code-to-CharProc-stream map, per-code advance widths,
//! and the font's own `/Resources` (for names referenced inside a CharProc).
//!
//! Painting itself -- running each CharProc as a nested content stream -- is
//! a later plan; this module only parses the font dictionary.

use pdfboss_core::FastMap;

use pdfboss_core::geom::Matrix;
use pdfboss_core::{Dict, Document, Object};

use crate::glyph::differences;

/// A parsed `/Type3` font dictionary: everything the (later) CharProc
/// executor needs, short of actually running a CharProc's content stream.
pub(crate) struct Type3Font {
    /// Glyph space -> text space. Type3 has no default matrix (unlike the
    /// outline fonts' implicit 1000-upm space), so a font with no usable
    /// `/FontMatrix` fails to load entirely -- see `load`.
    font_matrix: Matrix,
    /// code -> glyph name, from `/Encoding /Differences`, then a
    /// StandardEncoding base-name fallback restricted to the cases documented
    /// on `load`. `None` where the code is unmapped.
    encoding: Box<[Option<String>; 256]>,
    /// glyph name -> its CharProc stream object. Stored as given (possibly an
    /// indirect reference) and resolved lazily by the executor at paint time.
    char_procs: FastMap<String, Object>,
    /// code -> advance width in GLYPH space (`/Widths` + `/FirstChar`,
    /// unscaled by `/1000` and unscaled by `font_matrix` -- both happen at
    /// paint time). Absent for any code the font declared no width for.
    widths: FastMap<u32, f32>,
    /// The font's own `/Resources` dictionary (used to resolve names
    /// referenced inside its CharProcs), or `None` if the font dict has none
    /// -- the executor then falls back to the surrounding resource chain.
    resources: Option<Dict>,
}

impl Type3Font {
    /// Loads a `/Type3` font dict, or `None` if it lacks a usable
    /// `/FontMatrix` or a non-empty `/CharProcs`. Never panics: every access
    /// is `Option`/bounds-checked, so a malformed `/Widths`, `/CharProcs`
    /// entry, or `/Encoding` degrades to fewer mappings rather than failing
    /// the whole load (except the two required entries above).
    pub(crate) fn load(doc: &Document, font: &Dict) -> Option<Type3Font> {
        let font_matrix = parse_font_matrix(doc, font)?;
        let char_procs = parse_char_procs(doc, font)?;
        let encoding = parse_encoding(doc, font);
        let widths = parse_widths(doc, font);
        let resources = font
            .get("Resources")
            .and_then(|o| doc.resolve(o).ok())
            .and_then(|o| o.as_dict().cloned());

        Some(Type3Font {
            font_matrix,
            encoding,
            char_procs,
            widths,
            resources,
        })
    }

    /// The CharProc stream object for character `code`, or `None` if `code`
    /// is unmapped by `/Encoding` or names a glyph absent from `/CharProcs`.
    pub(crate) fn char_proc(&self, code: u32) -> Option<&Object> {
        let name = usize::try_from(code)
            .ok()
            .and_then(|i| self.encoding.get(i))?
            .as_ref()?;
        self.char_procs.get(name)
    }

    /// The glyph-space advance width for `code`, or `None` if the font
    /// declared no `/Widths` entry for it.
    pub(crate) fn width(&self, code: u32) -> Option<f32> {
        self.widths.get(&code).copied()
    }

    /// The font's `/FontMatrix` (glyph space -> text space).
    pub(crate) fn font_matrix(&self) -> Matrix {
        self.font_matrix
    }

    /// The font's own `/Resources` dict, or `None`.
    pub(crate) fn resources(&self) -> Option<&Dict> {
        self.resources.as_ref()
    }
}

/// Parses `/FontMatrix` into a [`Matrix`]: a 6-number array, every entry
/// resolved and finite. `None` on anything else (missing, wrong length,
/// unresolvable entry, or a non-finite value) -- Type3 has no default matrix
/// to fall back to.
fn parse_font_matrix(doc: &Document, font: &Dict) -> Option<Matrix> {
    let resolved = doc.resolve(font.get("FontMatrix")?).ok()?;
    let items = resolved.as_array()?;
    if items.len() != 6 {
        return None;
    }
    let mut nums = [0.0f64; 6];
    for (slot, item) in nums.iter_mut().zip(items) {
        let v = doc.resolve(item).ok()?.as_f64()?;
        if !v.is_finite() {
            return None;
        }
        *slot = v;
    }
    Some(Matrix {
        a: nums[0] as f32,
        b: nums[1] as f32,
        c: nums[2] as f32,
        d: nums[3] as f32,
        e: nums[4] as f32,
        f: nums[5] as f32,
    })
}

/// Parses `/CharProcs` into a glyph-name -> (unresolved) stream-object map.
/// `None` if `/CharProcs` is absent, not a dictionary, or resolves to an
/// empty dictionary -- a Type3 font with no CharProcs can paint nothing.
fn parse_char_procs(doc: &Document, font: &Dict) -> Option<FastMap<String, Object>> {
    let resolved = doc.resolve(font.get("CharProcs")?).ok()?;
    let dict = resolved.as_dict()?;
    if dict.is_empty() {
        return None;
    }
    let map: FastMap<String, Object> = dict
        .iter()
        .map(|(name, obj)| (name.0.clone(), obj.clone()))
        .collect();
    Some(map)
}

/// Builds the 256-entry code -> glyph-name table: `/Encoding /Differences`
/// entries (via [`differences`]) take priority; codes it leaves unmapped fall
/// back to `pdfboss_encoding::standard_encoding_name` only when `/Encoding`
/// has no `/BaseEncoding` entry, or an explicit `/BaseEncoding
/// /StandardEncoding` -- Type3's implicit base per ISO 32000-1 §9.6.5.2. A
/// `/BaseEncoding` naming anything else leaves those codes `None`; this is a
/// documented v1 limit, not a panic path.
fn parse_encoding(doc: &Document, font: &Dict) -> Box<[Option<String>; 256]> {
    let diffs = differences(doc, font);
    let fallback = allows_standard_fallback(doc, font);

    let mut table: Box<[Option<String>; 256]> = Box::new(std::array::from_fn(|_| None));
    for (code, slot) in table.iter_mut().enumerate() {
        let code = code as u8;
        if let Some(name) = diffs.get(&code) {
            *slot = Some(name.clone());
        } else if fallback {
            *slot = pdfboss_encoding::standard_encoding_name(code).map(str::to_string);
        }
    }
    table
}

/// Whether the StandardEncoding base-name fallback applies: true when
/// `/Encoding` is absent, is a dictionary with no `/BaseEncoding` key, or has
/// `/BaseEncoding /StandardEncoding`; false when `/BaseEncoding` names
/// anything else.
fn allows_standard_fallback(doc: &Document, font: &Dict) -> bool {
    match font.get("Encoding").map(|o| doc.resolve(o)) {
        None => true,
        Some(Ok(Object::Dict(d))) => match d.get_name("BaseEncoding") {
            None => true,
            Some(name) => name.0 == "StandardEncoding",
        },
        _ => true,
    }
}

/// Parses `/Widths` + `/FirstChar` into a code -> glyph-space-width map
/// (mirrors `glyph.rs::simple_widths`'s shape, but keeps the raw glyph-space
/// number -- no `/1000` scaling, which happens at paint time instead).
fn parse_widths(doc: &Document, font: &Dict) -> FastMap<u32, f32> {
    let first = font
        .get("FirstChar")
        .and_then(|o| doc.resolve(o).ok())
        .and_then(|o| o.as_int())
        .unwrap_or(0)
        .max(0) as u32;

    let mut map = FastMap::default();
    if let Some(Ok(Object::Array(items))) = font.get("Widths").map(|o| doc.resolve(o)) {
        for (i, item) in items.iter().enumerate() {
            let Some(code) = first.checked_add(i as u32) else {
                break; // /FirstChar so large the codes overflow u32
            };
            if let Some(w) = doc.resolve(item).ok().and_then(|o| o.as_f64()) {
                map.insert(code, w as f32);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use pdfboss_core::{Dict, Document, ObjRef};
    use pdfboss_testkit::PdfBuilder;

    use super::Type3Font;

    /// Builds a one-object synthetic `/Type3` font (object 3): `/FontMatrix
    /// [0.001 0 0 0.001 0 0]`, `/Encoding /Differences [65 /boxglyph]`,
    /// `/CharProcs << /boxglyph 2 0 R >>` (an ASCII CharProc stream that
    /// paints a `d0`-declared box), `/FirstChar 65 /Widths [1000]`. Returns
    /// the loaded `Document` and the resolved font `Dict`.
    fn type3_font_doc() -> (Document, Dict) {
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog >>");
        b.stream(2, "", b"1000 0 d0 100 0 500 700 re f");
        b.object(
            3,
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
             /FontMatrix [0.001 0 0 0.001 0 0] \
             /Encoding << /Differences [65 /boxglyph] >> \
             /CharProcs << /boxglyph 2 0 R >> \
             /FirstChar 65 /Widths [1000] >>",
        );
        let bytes = b.build(1);
        let doc = Document::load(bytes).expect("load");
        let font_dict = doc
            .get(ObjRef { num: 3, gen: 0 })
            .expect("resolve font dict")
            .as_dict()
            .cloned()
            .expect("font dict");
        (doc, font_dict)
    }

    #[test]
    fn load_resolves_charproc_width_and_matrix() {
        let (doc, font_dict) = type3_font_doc();
        let t3 = Type3Font::load(&doc, &font_dict).expect("load");
        assert!(t3.char_proc(65).is_some(), "code 65 -> /boxglyph CharProc");
        assert_eq!(t3.width(65), Some(1000.0)); // glyph-space width, unscaled
        let m = t3.font_matrix();
        assert!((m.a - 0.001).abs() < 1e-9 && (m.d - 0.001).abs() < 1e-9);
        assert!(t3.char_proc(66).is_none(), "unmapped code -> None");
    }

    #[test]
    fn load_without_font_matrix_returns_none() {
        // A /Type3 dict missing /FontMatrix cannot map glyph space -> text
        // space -- unlike the outline fonts, Type3 has no default matrix.
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog >>");
        b.stream(2, "", b"1000 0 d0 100 0 500 700 re f");
        b.object(
            3,
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
             /Encoding << /Differences [65 /boxglyph] >> \
             /CharProcs << /boxglyph 2 0 R >> \
             /FirstChar 65 /Widths [1000] >>",
        );
        let bytes = b.build(1);
        let doc = Document::load(bytes).expect("load");
        let font_dict = doc
            .get(ObjRef { num: 3, gen: 0 })
            .expect("resolve font dict")
            .as_dict()
            .cloned()
            .expect("font dict");
        assert!(Type3Font::load(&doc, &font_dict).is_none());
    }

    #[test]
    fn load_tolerates_malformed_widths_and_charprocs() {
        // /Widths is a Name, not an array; the /CharProcs entry for
        // /boxglyph is a plain (non-stream) dict entry rather than a stream
        // object. Neither should panic; a usable /FontMatrix and non-empty
        // /CharProcs dict still make `load` succeed, just with `width`
        // returning `None` for every code.
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog >>");
        b.object(
            3,
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
             /FontMatrix [0.001 0 0 0.001 0 0] \
             /Encoding << /Differences [65 /boxglyph] >> \
             /CharProcs << /boxglyph /NotAStream >> \
             /FirstChar 65 /Widths /NotAnArray >>",
        );
        let bytes = b.build(1);
        let doc = Document::load(bytes).expect("load");
        let font_dict = doc
            .get(ObjRef { num: 3, gen: 0 })
            .expect("resolve font dict")
            .as_dict()
            .cloned()
            .expect("font dict");

        // Must not panic; the loader is free to return Some (with the
        // malformed entries dropped/degraded) or None -- either is fine, as
        // long as it doesn't crash.
        let result = Type3Font::load(&doc, &font_dict);
        if let Some(t3) = result {
            assert_eq!(t3.width(65), None, "malformed /Widths yields no widths");
        }
    }
}
