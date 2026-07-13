//! Bridges a PDF font dictionary to embedded TrueType outlines for painting.
//!
//! Supports simple `/TrueType` fonts and `/Type0` composite fonts with a
//! `CIDFontType2` descendant, both carrying an embedded `FontFile2` program.
//! Every other font (Type1/CFF programs, the standard 14, non-embedded fonts)
//! yields `None`, so the renderer leaves that text unpainted rather than
//! guessing.

use pdfboss_core::{Dict, Document, Object};

use crate::truetype::{Seg, TrueType};

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
    tt: TrueType,
    kind: GlyphKind,
}

impl GlyphFont {
    /// Loads paintable glyph data from a (resolved) font dictionary, or `None`
    /// if the font is not an embedded TrueType.
    pub(crate) fn load(doc: &Document, font: &Dict) -> Option<GlyphFont> {
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
        self.tt.glyph_path(gid)
    }

    /// The glyph's advance width in font units.
    pub(crate) fn advance(&self, gid: u16) -> u16 {
        self.tt.advance(gid)
    }

    /// Font design units per em (outline coordinate scale).
    pub(crate) fn units_per_em(&self) -> f32 {
        self.tt.units_per_em() as f32
    }
}

/// Loads a simple `/TrueType` font, building a code-to-glyph table by mapping
/// each byte to a code point (ASCII/Latin-1) and, failing that, the symbol
/// range `0xF000 + code`, through the font's `cmap`.
fn load_simple(doc: &Document, font: &Dict) -> Option<GlyphFont> {
    let descriptor = resolve_dict(doc, font.get("FontDescriptor")?)?;
    let program = stream_bytes(doc, descriptor.get("FontFile2")?)?;
    let tt = TrueType::parse(program)?;

    let mut table = Box::new([0u16; 256]);
    if tt.has_cmap() {
        for (code, slot) in table.iter_mut().enumerate() {
            let cp = code as u32;
            let mut gid = tt.gid_for_unicode(cp).unwrap_or(0);
            if gid == 0 {
                gid = tt.gid_for_unicode(0xF000 + cp).unwrap_or(0);
            }
            *slot = gid;
        }
    }
    Some(GlyphFont {
        tt,
        kind: GlyphKind::Simple(table),
    })
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
        tt,
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
