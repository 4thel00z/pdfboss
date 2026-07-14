//! Bridges a PDF font dictionary to embedded TrueType outlines for painting.
//!
//! Supports simple `/TrueType` fonts and `/Type0` composite fonts with a
//! `CIDFontType2` descendant, both carrying an embedded `FontFile2` program.
//! Every other font (Type1/CFF programs, the standard 14, non-embedded fonts)
//! yields `None`, so the renderer leaves that text unpainted rather than
//! guessing.

use std::collections::HashMap;

use pdfboss_core::{Dict, Document, Object};

use crate::truetype::{Seg, TrueType};
use crate::GlyphPainting;

/// Where a font's glyph outlines and metrics come from.
///
/// Today the only source is an embedded TrueType (`glyf`) program. The CFF,
/// Type1, Type3, and substitute-face loaders specified for later plans add
/// variants here, and the delegating methods below gain matching arms. This is
/// the single outline-source seam, which is why `GlyphFont`'s public surface
/// stays fixed as those loaders land.
enum Outlines {
    /// An embedded TrueType (`glyf`) program.
    TrueType(TrueType),
}

/// How character codes map to glyph indices for a loaded font.
enum GlyphKind {
    /// Simple font: one byte per code, mapped through this 256-entry table.
    Simple(Box<[u16; 256]>),
    /// `CIDFontType2`: two bytes per code (a CID). `None` is the identity
    /// CID-to-GID map; `Some` is an explicit table indexed by CID.
    Cid(Option<Vec<u16>>),
}

/// A font whose glyph outlines can be drawn.
pub(crate) struct GlyphFont {
    outlines: Outlines,
    kind: GlyphKind,
}

impl GlyphFont {
    /// Loads paintable glyph data from a (resolved) font dictionary, or `None`
    /// if the font is not an embedded TrueType.
    pub(crate) fn load(doc: &Document, font: &Dict, painting: GlyphPainting) -> Option<GlyphFont> {
        // Embedded TrueType is painted at every tier. Higher tiers will add
        // branches here: CFF/Type1/Type3 at `AllEmbedded`+, substitution at
        // `Full`. Until those loaders exist the tier changes nothing.
        let _ = painting;
        match font.get_name("Subtype").map(|n| n.0.as_str()) {
            Some("Type0") => load_type0(doc, font),
            Some("TrueType") => load_simple(doc, font),
            _ => None,
        }
    }

    /// Whether codes are two bytes wide (composite fonts).
    pub(crate) fn two_byte(&self) -> bool {
        matches!(self.kind, GlyphKind::Cid(_))
    }

    /// The glyph index for a character code.
    pub(crate) fn gid(&self, code: u32) -> u16 {
        match &self.kind {
            GlyphKind::Simple(table) => table[(code & 0xff) as usize],
            GlyphKind::Cid(None) => code as u16,
            GlyphKind::Cid(Some(map)) => map.get(code as usize).copied().unwrap_or(0),
        }
    }

    /// The glyph's outline as path segments in font units.
    pub(crate) fn outline(&self, gid: u16) -> Vec<Seg> {
        match &self.outlines {
            Outlines::TrueType(tt) => tt.glyph_path(gid),
        }
    }

    /// The glyph's advance width in font units.
    pub(crate) fn advance(&self, gid: u16) -> u16 {
        match &self.outlines {
            Outlines::TrueType(tt) => tt.advance(gid),
        }
    }

    /// Font design units per em (outline coordinate scale).
    pub(crate) fn units_per_em(&self) -> f32 {
        match &self.outlines {
            Outlines::TrueType(tt) => tt.units_per_em() as f32,
        }
    }
}

/// Loads a simple `/TrueType` font, building its 256-entry code-to-glyph table
/// by resolving each code in three tiers: a `/Differences` glyph name (via the
/// `post` table, then the Adobe Glyph List: name -> Unicode -> `cmap`); then the
/// base `/Encoding` character -> `cmap`; and finally the raw byte, then the
/// symbol range `0xF000 + code`, through the font's `cmap`.
fn load_simple(doc: &Document, font: &Dict) -> Option<GlyphFont> {
    let descriptor = resolve_dict(doc, font.get("FontDescriptor")?)?;
    let program = stream_bytes(doc, descriptor.get("FontFile2")?)?;
    let tt = TrueType::parse(program)?;

    let base = base_encoding(doc, font);
    let diffs = differences(doc, font);

    let mut table = Box::new([0u16; 256]);
    for (code, slot) in table.iter_mut().enumerate() {
        let code = code as u8;
        // 1. A /Differences name takes priority (post table, then glyph list).
        if let Some(name) = diffs.get(&code) {
            if let Some(gid) = resolve_name(&tt, name) {
                *slot = gid;
                continue;
            }
        }
        // 2. The base encoding gives a character to look up in the cmap.
        if let Some(ch) = base.and_then(|f| f(code)) {
            if let Some(gid) = tt.gid_for_unicode(ch as u32).filter(|&g| g != 0) {
                *slot = gid;
                continue;
            }
        }
        // 3. Fallback: the raw byte, then the symbol PUA range 0xF000+code.
        if tt.has_cmap() {
            let cp = u32::from(code);
            let mut gid = tt.gid_for_unicode(cp).unwrap_or(0);
            if gid == 0 {
                gid = tt.gid_for_unicode(0xF000 + cp).unwrap_or(0);
            }
            *slot = gid;
        }
    }
    Some(GlyphFont {
        outlines: Outlines::TrueType(tt),
        kind: GlyphKind::Simple(table),
    })
}

/// Resolves a glyph name to a glyph id: the font's `post` table first, then the
/// Adobe Glyph List (name → Unicode) through the `cmap`. Glyph id 0 (`.notdef`)
/// counts as "not found" so resolution can fall through.
fn resolve_name(tt: &TrueType, name: &str) -> Option<u16> {
    if let Some(gid) = tt.gid_for_name(name).filter(|&g| g != 0) {
        return Some(gid);
    }
    let ch = pdfboss_encoding::glyph_to_unicode(name)?;
    tt.gid_for_unicode(ch as u32).filter(|&g| g != 0)
}

/// Selects the base-encoding accessor (code → char) from a font's `/Encoding`
/// name or its `/BaseEncoding`. Returns `None` when the font has no `/Encoding`
/// (leaving the raw-byte fallback in charge, as before).
fn base_encoding(doc: &Document, font: &Dict) -> Option<fn(u8) -> Option<char>> {
    let name = match font.get("Encoding").map(|o| doc.resolve(o)) {
        Some(Ok(Object::Name(n))) => n.0,
        Some(Ok(Object::Dict(d))) => d
            .get_name("BaseEncoding")
            .map(|n| n.0.clone())
            .unwrap_or_else(|| "StandardEncoding".to_string()),
        _ => return None,
    };
    Some(match name.as_str() {
        "WinAnsiEncoding" => pdfboss_encoding::win_ansi,
        "MacRomanEncoding" => pdfboss_encoding::mac_roman,
        _ => pdfboss_encoding::standard,
    })
}

/// Parses `/Encoding /Differences` into a code → glyph-name map (empty when
/// `/Encoding` is not a dictionary or has no `/Differences`).
fn differences(doc: &Document, font: &Dict) -> HashMap<u8, String> {
    let mut out = HashMap::new();
    let Some(Ok(Object::Dict(enc))) = font.get("Encoding").map(|o| doc.resolve(o)) else {
        return out;
    };
    let Some(Ok(arr)) = enc.get("Differences").map(|o| doc.resolve(o)) else {
        return out;
    };
    let Some(items) = arr.as_array() else {
        return out;
    };
    let mut code: i64 = 0;
    for item in items {
        match item {
            Object::Int(n) => code = *n,
            Object::Name(name) => {
                if (0..256).contains(&code) {
                    out.insert(code as u8, name.0.clone());
                }
                code = code.saturating_add(1);
            }
            _ => {}
        }
    }
    out
}

/// Loads a `/Type0` font with a `CIDFontType2` descendant (embedded
/// TrueType), reading its `/CIDToGIDMap`. Codes are assumed two bytes
/// (`Identity-H`/`Identity-V` encoding, the embedded-subset norm).
fn load_type0(doc: &Document, font: &Dict) -> Option<GlyphFont> {
    let descendants = doc.resolve(font.get("DescendantFonts")?).ok()?;
    let first = descendants.as_array()?.first()?;
    let cid = resolve_dict(doc, first)?;
    if cid.get_name("Subtype").map(|n| n.0.as_str()) != Some("CIDFontType2") {
        return None; // CIDFontType0 is CFF, not glyf
    }
    let descriptor = resolve_dict(doc, cid.get("FontDescriptor")?)?;
    let program = stream_bytes(doc, descriptor.get("FontFile2")?)?;
    let tt = TrueType::parse(program)?;

    // /CIDToGIDMap: /Identity (or absent) means GID == CID; a stream is a
    // big-endian u16 table indexed by CID.
    let map = match cid.get("CIDToGIDMap").map(|o| doc.resolve(o)) {
        Some(Ok(Object::Stream(s))) => {
            let bytes = doc.stream_data(&s).ok()?;
            Some(
                bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect(),
            )
        }
        _ => None, // Identity
    };
    Some(GlyphFont {
        outlines: Outlines::TrueType(tt),
        kind: GlyphKind::Cid(map),
    })
}

/// Resolves an object to an owned dictionary.
fn resolve_dict(doc: &Document, obj: &Object) -> Option<Dict> {
    doc.resolve(obj).ok()?.as_dict().cloned()
}

/// Resolves an object to a stream and returns its decoded bytes.
fn stream_bytes(doc: &Document, obj: &Object) -> Option<Vec<u8>> {
    match doc.resolve(obj).ok()? {
        Object::Stream(s) => doc.stream_data(&s).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use pdfboss_core::Document;
    use pdfboss_testkit::PdfBuilder;

    use crate::truetype::tests::build_font;

    /// Builds a one-page PDF showing `content` with a simple `/TrueType` font
    /// (the synthetic `build_font` program) and the given `/Encoding` entry.
    fn simple_font_doc(encoding: &str, content: &[u8]) -> Vec<u8> {
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", content);
        b.object(
            5,
            &format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /X \
                 /FontDescriptor 6 0 R {encoding} >>"
            ),
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile2 7 0 R >>",
        );
        b.stream(7, "", &build_font());
        b.build(1)
    }

    /// The rectangle glyph (gid 1) is painted iff a dark pixel lands at (55,115),
    /// matching the geometry asserted in `truetype`'s render tests.
    fn glyph_painted(bytes: Vec<u8>) -> bool {
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let pix = crate::render_page(&doc, &page, 1.0).expect("render");
        let o = ((115 * pix.width + 55) * 4) as usize;
        pix.data[o] < 128 && pix.data[o + 1] < 128 && pix.data[o + 2] < 128
    }

    #[test]
    fn differences_name_paints_via_post() {
        // Code 0x80 is unmapped by the font cmap; only the /Differences name
        // "foo", resolved through the post table, reaches glyph 1.
        let doc = simple_font_doc(
            "/Encoding << /Differences [128 /foo] >>",
            b"BT /F0 100 Tf 20 50 Td <80> Tj ET",
        );
        assert!(
            glyph_painted(doc),
            "glyph should paint via /Differences+post"
        );
    }

    #[test]
    fn base_encoding_letter_still_paints() {
        // With WinAnsiEncoding, code 0x41 ('A') resolves through the cmap to
        // glyph 1 — the base-encoding path.
        let doc = simple_font_doc(
            "/Encoding /WinAnsiEncoding",
            b"BT /F0 100 Tf 20 50 Td <41> Tj ET",
        );
        assert!(
            glyph_painted(doc),
            "letter A should paint via the base encoding"
        );
    }

    #[test]
    fn differences_with_huge_code_does_not_panic() {
        // A hostile /Differences code at i64::MAX must not overflow `code += 1`.
        // The out-of-range code is ignored; rendering must complete and 'A'
        // (0x41, via the base encoding) still paints.
        let doc = simple_font_doc(
            "/Encoding << /Differences [9223372036854775807 /foo] >>",
            b"BT /F0 100 Tf 20 50 Td <41> Tj ET",
        );
        assert!(
            glyph_painted(doc),
            "render must complete without overflow panic"
        );
    }
}
