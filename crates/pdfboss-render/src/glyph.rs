//! Bridges a PDF font dictionary to embedded glyph outlines for painting.
//!
//! Supports simple `/TrueType` fonts and `/Type0` composite fonts with a
//! `CIDFontType2` descendant (an embedded `FontFile2` program) at every
//! [`GlyphPainting`] tier, plus simple `/Type1`/`/MMType1` fonts (either an
//! embedded CFF `FontFile3` program or a real Type1 charstring `FontFile`
//! program) and `CIDFontType0` descendants carrying an embedded CFF
//! `FontFile3` program once the tier reaches `AllEmbedded`. At the `Full`
//! tier, a SIMPLE font with no embedded program of its own (a non-embedded
//! `/TrueType`, `/Type1`, or `/MMType1` font, including the standard 14)
//! instead substitutes a provider-supplied TrueType face
//! (`crate::substitute`), with Adobe Core-14 AFM metrics filling in for a
//! missing `/Widths`. Substitution is scoped to simple font subtypes only:
//! a `/Type0` composite font's codes are two bytes wide and a substitute's
//! 1-byte table would mis-split them, so a non-embedded `/Type0` never
//! substitutes, at any tier. `/Type3` (whose glyphs paint via
//! `/CharProcs`, handled by the executor re-entering itself, not this
//! module) and any font that still yields no result (Symbol/ZapfDingbats,
//! or `Full` with no substitute provider) leave that text unpainted rather
//! than guessing.

use std::collections::HashMap;

use pdfboss_core::{Dict, Document, Object};

use crate::cff::CffFont;
use crate::substitute::{FaceRequest, SubstituteProvider};
use crate::truetype::{Seg, TrueType};
use crate::type1::Type1Font;
use crate::GlyphPainting;

/// Where a font's glyph outlines and metrics come from.
///
/// The Type3 loader specified for a later plan adds a further variant here,
/// and the delegating methods below gain a matching arm. This is the single
/// outline-source seam, which is why `GlyphFont`'s public surface stays
/// fixed as that loader lands.
enum Outlines {
    /// An embedded TrueType (`glyf`) program.
    TrueType(TrueType),
    /// An embedded CFF (`Type1C`/`CIDFontType0C`) program.
    Cff(CffFont),
    /// An embedded Type1 charstring program (`FontFile`).
    Type1(Type1Font),
    /// A non-embedded font's provider-supplied substitute TrueType face
    /// (`Full`-tier substitution), standing in for the original font, which
    /// has no glyph program of its own.
    Substitute(TrueType),
}

/// How character codes map to glyph indices for a loaded font.
enum GlyphKind {
    /// Simple font: one byte per code, mapped through this 256-entry table.
    Simple(Box<[u16; 256]>),
    /// `CIDFontType2`: two bytes per code (a CID). `None` is the identity
    /// CID-to-GID map; `Some` is an explicit table indexed by CID.
    Cid(Option<Vec<u16>>),
}

/// Advance widths declared by the PDF font dictionary itself: `/Widths` +
/// `/FirstChar` for simple fonts, `/W` + `/DW` for Type0/CID fonts. Keyed by
/// character code (simple) or CID (Type0, under the identity encoding these
/// loaders assume), holding advances in the PDF's 1000-unit glyph space.
///
/// `declared` distinguishes "this font specified widths, and `default`
/// governs any code without its own entry" from "this font specified no
/// width information at all" -- the latter is common for simple TrueType
/// fonts, which rely on the embedded program's own `hmtx` table instead, so
/// `GlyphFont::advance` must fall back to the program advance rather than
/// treating every code as width 0.
struct WidthMap {
    map: HashMap<u32, f32>,
    default: f32,
    declared: bool,
}

impl WidthMap {
    /// The PDF-declared advance (1000-unit glyph space) for `code`, or
    /// `None` if this font declared no width information at all.
    fn get(&self, code: u32) -> Option<f32> {
        self.declared
            .then(|| self.map.get(&code).copied().unwrap_or(self.default))
    }
}

/// A font whose glyph outlines can be drawn.
pub(crate) struct GlyphFont {
    outlines: Outlines,
    kind: GlyphKind,
    widths: WidthMap,
    /// A second, per-code-optional advance tier consulted between `widths`
    /// and each program's own advance metric: Adobe Core-14 AFM widths,
    /// populated only by `load_substitute`, and only for a recognized
    /// standard-14 `/BaseFont` (empty for every other loader, and for a
    /// substitute font that isn't one of the standard 14 -- so this tier is
    /// a no-op everywhere except non-embedded standard-14 substitution).
    /// Unlike `WidthMap`, a missing code here means "no AFM entry for this
    /// code", not "declared width 0" -- see `GlyphFont::advance`.
    afm_widths: HashMap<u32, f32>,
}

impl GlyphFont {
    /// Loads paintable glyph data from a (resolved) font dictionary, or
    /// `None` if the font has no loader for its `/Subtype` at this
    /// `painting` tier and (at `Full`) no usable substitute either.
    ///
    /// `provider` is the `Full`-tier substitute source (from
    /// [`crate::RenderOptions::substitutes`]). An embedded program always
    /// takes precedence when the font actually carries one -- substitution
    /// is strictly the non-embedded last resort, tried only once every
    /// embedded loader below has declined.
    pub(crate) fn load(
        doc: &Document,
        font: &Dict,
        painting: GlyphPainting,
        provider: Option<&dyn SubstituteProvider>,
    ) -> Option<GlyphFont> {
        // Embedded TrueType paints at every tier. CFF and Type1 (simple
        // Type1/MMType1 fonts, and CIDFontType0 descendants for CFF) join at
        // `AllEmbedded`+. `Full`-tier substitution (`substitute_at_full`) is
        // chained only onto the SIMPLE font arms (TrueType, Type1,
        // MMType1): `Type0` never substitutes (its codes are two bytes wide;
        // a substitute's 1-byte table would mis-split them), and `Type3`
        // (the executor's `/CharProcs` path) falls into the `_` catch-all,
        // which also never substitutes.
        match font.get_name("Subtype").map(|n| n.0.as_str()) {
            Some("Type0") => load_type0(doc, font, painting),
            Some("TrueType") => {
                load_simple(doc, font).or_else(|| substitute_at_full(doc, font, painting, provider))
            }
            Some("Type1") | Some("MMType1") if painting.paints_all_embedded() => {
                load_simple_type1_or_cff(doc, font)
                    .or_else(|| substitute_at_full(doc, font, painting, provider))
            }
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
            Outlines::TrueType(tt) | Outlines::Substitute(tt) => tt.glyph_path(gid),
            Outlines::Cff(cff) => cff.glyph_path(gid),
            Outlines::Type1(t1) => t1.glyph_path(gid),
        }
    }

    /// The advance width for character `code`, in font units. Three tiers,
    /// most authoritative first: the PDF's own declared width (`/Widths`
    /// for simple fonts, `/W`+`/DW` for Type0/CID); failing that, `afm_widths`
    /// (Adobe Core-14 AFM metrics, populated only for non-embedded
    /// standard-14 substitution -- empty, and so a no-op, everywhere else);
    /// and only failing both does this fall back to the embedded program's
    /// own advance metric (`hmtx` for TrueType and for a substitute face,
    /// `0` for CFF and Type1, neither of which has a per-glyph advance table
    /// parsed here -- Type1's `hsbw`/`sbw` operator does carry a `wx`
    /// advance operand, but reading it back out of the charstring is a
    /// deferred fallback, not yet wired up).
    pub(crate) fn advance(&self, code: u32) -> f32 {
        if let Some(width_1000) = self.widths.get(code) {
            return width_1000 / 1000.0 * self.units_per_em();
        }
        if let Some(&width_1000) = self.afm_widths.get(&code) {
            return width_1000 / 1000.0 * self.units_per_em();
        }
        match &self.outlines {
            Outlines::TrueType(tt) | Outlines::Substitute(tt) => {
                f32::from(tt.advance(self.gid(code)))
            }
            Outlines::Cff(_) | Outlines::Type1(_) => 0.0,
        }
    }

    /// Font design units per em (outline coordinate scale).
    pub(crate) fn units_per_em(&self) -> f32 {
        match &self.outlines {
            Outlines::TrueType(tt) | Outlines::Substitute(tt) => tt.units_per_em() as f32,
            Outlines::Cff(cff) => cff.units_per_em(),
            Outlines::Type1(t1) => t1.units_per_em(),
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
        widths: simple_widths(doc, font),
        afm_widths: HashMap::new(),
    })
}

/// Parses a simple font's `/Widths` + `/FirstChar` (keyed by character
/// code) and the `/FontDescriptor /MissingWidth` default. `declared` is true
/// iff `/Widths` is an array, regardless of whether any entry resolves --
/// once a font declares widths, an unresolved entry still falls back to
/// `default`, not to the embedded program's own advance.
fn simple_widths(doc: &Document, font: &Dict) -> WidthMap {
    let first = font
        .get("FirstChar")
        .and_then(|o| doc.resolve(o).ok())
        .and_then(|o| o.as_int())
        .unwrap_or(0)
        .max(0) as u32;

    let mut map = HashMap::new();
    let mut declared = false;
    if let Some(Ok(Object::Array(items))) = font.get("Widths").map(|o| doc.resolve(o)) {
        declared = true;
        for (i, item) in items.iter().enumerate() {
            let Some(code) = first.checked_add(i as u32) else {
                break; // /FirstChar so large the codes overflow u32
            };
            if let Some(w) = doc.resolve(item).ok().and_then(|o| o.as_f64()) {
                map.insert(code, w as f32);
            }
        }
    }

    let default = font
        .get("FontDescriptor")
        .and_then(|o| doc.resolve(o).ok())
        .and_then(|o| o.as_dict().cloned())
        .and_then(|fd| fd.get("MissingWidth").and_then(|o| doc.resolve(o).ok()))
        .and_then(|o| o.as_f64())
        .map(|w| w as f32)
        .unwrap_or(0.0);

    WidthMap {
        map,
        default,
        declared,
    }
}

/// Loads a simple `/Type1`/`/MMType1` font whose `FontDescriptor` carries an
/// embedded CFF program (`FontFile3`). Dispatched from
/// `load_simple_type1_or_cff`, which tries this first; a descriptor with
/// `FontFile` instead (a raw Type1 charstring program, not CFF) goes to
/// `load_type1_simple`.
///
/// Builds its 256-entry code-to-glyph table from two sources, in priority
/// order: a `/Differences` glyph name, resolved directly through the CFF's
/// own charset (`gid_for_name`); then the base `/Encoding` character, looked
/// up in a `unicode -> gid` map built once by walking every glyph's charset
/// name through the Adobe Glyph List. CFF has no `cmap`, so unlike the
/// TrueType loader there is no raw-byte/symbol-range fallback: an unresolved
/// code is left at `.notdef` (gid 0).
fn load_cff_simple(doc: &Document, font: &Dict) -> Option<GlyphFont> {
    let descriptor = resolve_dict(doc, font.get("FontDescriptor")?)?;
    let program = stream_bytes(doc, descriptor.get("FontFile3")?)?;
    let cff = CffFont::parse(program)?;

    let mut by_unicode: HashMap<char, u16> = HashMap::new();
    for gid in 1..cff.num_glyphs() {
        // `num_glyphs` is bounded by the CharStrings INDEX's u16 count, so
        // this cast never truncates.
        let gid = gid as u16;
        let Some(name) = cff.name_for_gid(gid) else {
            continue;
        };
        if let Some(ch) = pdfboss_encoding::glyph_to_unicode(&name) {
            by_unicode.entry(ch).or_insert(gid);
        }
    }

    let base = base_encoding(doc, font);
    let diffs = differences(doc, font);

    let mut table = Box::new([0u16; 256]);
    for (code, slot) in table.iter_mut().enumerate() {
        let code = code as u8;
        // 1. A /Differences name, resolved via the CFF's own charset.
        if let Some(name) = diffs.get(&code) {
            if let Some(gid) = cff.gid_for_name(name).filter(|&g| g != 0) {
                *slot = gid;
                continue;
            }
        }
        // 2. The base encoding's character, via the unicode -> gid map.
        if let Some(ch) = base.and_then(|f| f(code)) {
            if let Some(&gid) = by_unicode.get(&ch) {
                *slot = gid;
            }
        }
    }
    Some(GlyphFont {
        outlines: Outlines::Cff(cff),
        kind: GlyphKind::Simple(table),
        widths: simple_widths(doc, font),
        afm_widths: HashMap::new(),
    })
}

/// Dispatches a simple `/Type1`/`/MMType1` font's `FontDescriptor` to
/// whichever embedded program it actually carries. CFF (`FontFile3`) wins
/// when present -- this is the pre-existing path, unchanged -- falling back
/// to a raw Type1 charstring program (`FontFile`) only when there is no
/// `FontFile3`. A descriptor with neither (or no descriptor at all) yields
/// `None`.
fn load_simple_type1_or_cff(doc: &Document, font: &Dict) -> Option<GlyphFont> {
    let descriptor = resolve_dict(doc, font.get("FontDescriptor")?)?;
    if descriptor.get("FontFile3").is_some() {
        return load_cff_simple(doc, font);
    }
    if descriptor.get("FontFile").is_some() {
        return load_type1_simple(doc, font);
    }
    None
}

/// Loads a simple `/Type1`/`/MMType1` font whose `FontDescriptor` carries a
/// raw Type1 charstring program (`FontFile`, decrypted and interpreted by
/// `type1.rs`). Dispatched from `load_simple_type1_or_cff` once `FontFile3`
/// (CFF) has been ruled out.
///
/// Builds its 256-entry code-to-glyph table from three sources, in priority
/// order: a `/Differences` glyph name, resolved directly through the font's
/// own name table (`gid_for_name`); then the base `/Encoding` character,
/// looked up in a `unicode -> gid` map built once by walking every glyph
/// name through the Adobe Glyph List (mirrors `load_cff_simple`'s
/// `by_unicode` construction exactly, just sourced from `Type1Font::
/// name_for_gid` instead of `CffFont::name_for_gid`); then -- a tier
/// `load_cff_simple` has no counterpart for, since CFF's charset carries no
/// separate built-in encoding -- the font's own built-in `/Encoding` array
/// (`builtin_name`), for a font that ships its own encoding and the PDF
/// gives none. Type1 has no `cmap`, so as with CFF an unresolved code is
/// left at `.notdef` (gid 0).
fn load_type1_simple(doc: &Document, font: &Dict) -> Option<GlyphFont> {
    let descriptor = resolve_dict(doc, font.get("FontDescriptor")?)?;
    let program = stream_bytes(doc, descriptor.get("FontFile")?)?;
    let t1 = Type1Font::parse(program)?;

    let mut by_unicode: HashMap<char, u16> = HashMap::new();
    for gid in 1..t1.num_glyphs() {
        // `num_glyphs` is bounded by `Type1Font::parse`'s `MAX_GLYPHS` cap
        // (65536), so this cast never truncates.
        let gid = gid as u16;
        let Some(name) = t1.name_for_gid(gid) else {
            continue;
        };
        if let Some(ch) = pdfboss_encoding::glyph_to_unicode(name) {
            by_unicode.entry(ch).or_insert(gid);
        }
    }

    let base = base_encoding(doc, font);
    let diffs = differences(doc, font);

    let mut table = Box::new([0u16; 256]);
    for (code, slot) in table.iter_mut().enumerate() {
        let code = code as u8;
        // 1. A /Differences name, resolved via the font's own name table.
        if let Some(name) = diffs.get(&code) {
            if let Some(gid) = t1.gid_for_name(name).filter(|&g| g != 0) {
                *slot = gid;
                continue;
            }
        }
        // 2. The base encoding's character, via the unicode -> gid map.
        if let Some(ch) = base.and_then(|f| f(code)) {
            if let Some(&gid) = by_unicode.get(&ch) {
                *slot = gid;
                continue;
            }
        }
        // 3. The font's own built-in /Encoding -- Type1-specific: a font
        // that ships its own /Encoding and gets no PDF /Encoding at all
        // still maps, unlike CFF (which has no built-in encoding concept).
        if let Some(name) = t1.builtin_name(code) {
            if let Some(gid) = t1.gid_for_name(name).filter(|&g| g != 0) {
                *slot = gid;
            }
        }
    }
    Some(GlyphFont {
        outlines: Outlines::Type1(t1),
        kind: GlyphKind::Simple(table),
        widths: simple_widths(doc, font),
        afm_widths: HashMap::new(),
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
pub(crate) fn differences(doc: &Document, font: &Dict) -> HashMap<u8, String> {
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

/// Whether `font`'s base encoding resolves to `StandardEncoding` -- named
/// explicitly, defaulted by an `/Encoding` dictionary with no
/// `/BaseEncoding`, or the ISO 32000-1 9.6.6 default when `/Encoding` is
/// absent entirely -- rather than `WinAnsiEncoding`/`MacRomanEncoding`.
/// Mirrors `base_encoding`'s own `/Encoding` walk, but yields a name-table
/// applicability bit instead of a char accessor: `load_substitute`'s AFM
/// lookup only has a code -> glyph-name table for `StandardEncoding`
/// (`pdfboss_encoding::standard_encoding_name`), not for WinAnsi or
/// MacRoman, so a WinAnsi/MacRoman code must not claim an AFM width through
/// this path (it falls through to the substitute's own `hmtx` instead).
fn is_standard_encoding(doc: &Document, font: &Dict) -> bool {
    let name = match font.get("Encoding").map(|o| doc.resolve(o)) {
        Some(Ok(Object::Name(n))) => n.0,
        Some(Ok(Object::Dict(d))) => d
            .get_name("BaseEncoding")
            .map(|n| n.0.clone())
            .unwrap_or_else(|| "StandardEncoding".to_string()),
        _ => return true, // no /Encoding at all -> defaults to Standard
    };
    !matches!(name.as_str(), "WinAnsiEncoding" | "MacRomanEncoding")
}

/// `Full`'s non-embedded last resort: substitutes a provider-supplied face.
/// Gated on both the tier and an actual provider, so `Full` with no
/// `SubstituteSource` behaves exactly like `AllEmbedded`.
///
/// Only ever chained onto a SIMPLE font subtype's arm in `GlyphFont::load`
/// (`TrueType`, `Type1`, `MMType1`) -- never called for `Type0` (whose
/// two-byte codes a 1-byte substitute table would mis-split) or `Type3`
/// (whose glyphs paint via `/CharProcs`, not an outline).
fn substitute_at_full(
    doc: &Document,
    font: &Dict,
    painting: GlyphPainting,
    provider: Option<&dyn SubstituteProvider>,
) -> Option<GlyphFont> {
    if painting == GlyphPainting::Full {
        if let Some(provider) = provider {
            return load_substitute(doc, font, provider);
        }
    }
    None
}

/// Loads a non-embedded font at the `Full` tier by substituting a
/// provider-supplied TrueType face -- the last resort tried once every
/// embedded loader above has declined (`GlyphFont::load` still prefers an
/// embedded program when the font actually carries one).
///
/// Builds its 256-entry code-to-glyph table with the same two tiers as
/// `load_simple`'s first two (a `/Differences` name, then the base
/// encoding's character), except both resolve through the Adobe Glyph List
/// into the SUBSTITUTE face's own `cmap` -- this font has no glyph program
/// of its own to resolve names or codes against, so there is no `post`-table
/// tier and no raw-byte/symbol-range fallback: a code that resolves to no
/// Unicode scalar, or one the substitute's `cmap` doesn't cover, is simply
/// left at `.notdef` (gid 0).
///
/// Advance widths add a middle tier (`GlyphFont::advance`'s `afm_widths`)
/// between the PDF's own `/Widths` and the substitute's `hmtx`: for a
/// recognized standard-14 `/BaseFont`, the Adobe Core-14 AFM width of the
/// code's glyph name (the `/Differences` name, else -- only when the base
/// encoding is `StandardEncoding` or absent -- the `StandardEncoding` name
/// for that code). A `WinAnsiEncoding`/`MacRomanEncoding` code has no name
/// table wired up here, so it simply gets no `afm_widths` entry and falls
/// through to the substitute's own `hmtx` -- metric-compatible with the
/// standard 14, so a near-identical advance even then.
fn load_substitute(
    doc: &Document,
    font: &Dict,
    provider: &dyn SubstituteProvider,
) -> Option<GlyphFont> {
    let req = FaceRequest::from_font_dict(doc, font)?;
    let bytes = provider.face(&req)?;
    let tt = TrueType::parse(bytes)?;

    let base = base_encoding(doc, font);
    let diffs = differences(doc, font);

    let mut table = Box::new([0u16; 256]);
    for (code, slot) in table.iter_mut().enumerate() {
        let code = code as u8;
        // 1. A /Differences name, through the glyph list, into the
        // SUBSTITUTE's own cmap.
        if let Some(name) = diffs.get(&code) {
            if let Some(ch) = pdfboss_encoding::glyph_to_unicode(name) {
                if let Some(gid) = tt.gid_for_unicode(ch as u32).filter(|&g| g != 0) {
                    *slot = gid;
                    continue;
                }
            }
        }
        // 2. The base encoding's character, via the substitute's cmap.
        if let Some(ch) = base.and_then(|f| f(code)) {
            if let Some(gid) = tt.gid_for_unicode(ch as u32).filter(|&g| g != 0) {
                *slot = gid;
            }
        }
    }

    let widths = simple_widths(doc, font);

    // AFM-14 advances: only for a recognized standard-14 /BaseFont. Codes
    // with no resolvable glyph name (WinAnsi/MacRoman, which have no
    // code -> name table here) are simply not inserted, so `advance` falls
    // through to the substitute's own hmtx for them.
    let base_font = font
        .get_name("BaseFont")
        .map(|n| n.0.as_str())
        .unwrap_or("");
    let mut afm_widths = HashMap::new();
    if pdfboss_encoding::is_standard_14(base_font) {
        let standard_ok = is_standard_encoding(doc, font);
        for code in 0u32..256 {
            let name = diffs.get(&(code as u8)).map(String::as_str).or_else(|| {
                standard_ok
                    .then(|| pdfboss_encoding::standard_encoding_name(code as u8))
                    .flatten()
            });
            if let Some(name) = name {
                if let Some(w) = pdfboss_encoding::standard_14_width(base_font, name) {
                    afm_widths.insert(code, w);
                }
            }
        }
    }

    Some(GlyphFont {
        outlines: Outlines::Substitute(tt),
        kind: GlyphKind::Simple(table),
        widths,
        afm_widths,
    })
}

/// Loads a `/Type0` composite font by dispatching on its descendant's
/// `/Subtype`: `CIDFontType2` (embedded TrueType) paints at every tier;
/// `CIDFontType0` (embedded CFF) joins once `painting` reaches `AllEmbedded`.
fn load_type0(doc: &Document, font: &Dict, painting: GlyphPainting) -> Option<GlyphFont> {
    let descendants = doc.resolve(font.get("DescendantFonts")?).ok()?;
    let first = descendants.as_array()?.first()?;
    let cid = resolve_dict(doc, first)?;
    match cid.get_name("Subtype").map(|n| n.0.as_str()) {
        Some("CIDFontType2") => load_type0_truetype(doc, &cid),
        Some("CIDFontType0") if painting.paints_all_embedded() => load_cff_cid(doc, &cid),
        _ => None,
    }
}

/// Loads a `CIDFontType2` descendant (embedded TrueType), reading its
/// `/CIDToGIDMap`. Codes are assumed two bytes (`Identity-H`/`Identity-V`
/// encoding, the embedded-subset norm).
fn load_type0_truetype(doc: &Document, cid: &Dict) -> Option<GlyphFont> {
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
        widths: cid_widths(doc, cid),
        afm_widths: HashMap::new(),
    })
}

/// Parses a descendant CID font's `/W` + `/DW`, keyed by CID (== the code
/// under the `Identity-H`/`Identity-V` encoding these loaders assume).
/// `declared` is true iff either key is present, so a descendant with
/// neither (uncommon, but not forbidden) leaves the fallback in charge.
fn cid_widths(doc: &Document, cid: &Dict) -> WidthMap {
    let mut default = 1000.0;
    let mut declared = false;
    if let Some(dw) = cid
        .get("DW")
        .and_then(|o| doc.resolve(o).ok())
        .and_then(|o| o.as_f64())
    {
        default = dw as f32;
        declared = true;
    }

    let mut map = HashMap::new();
    if let Some(Ok(Object::Array(items))) = cid.get("W").map(|o| doc.resolve(o)) {
        declared = true;
        parse_cid_width_array(doc, &items, &mut map);
    }

    WidthMap {
        map,
        default,
        declared,
    }
}

/// Hard ceiling on the number of CID width entries a single `/W` array may
/// insert in total. Each `c1 c2 w` RANGE is already capped at 65536 entries
/// on its own, but that alone doesn't bound the array as a whole: a crafted
/// `/W` array of many non-overlapping ranges (e.g. `0 65535 1  65536 131071
/// 1  ...`) can still expand to hundreds of millions of `HashMap` entries.
/// This caps the aggregate across every range/single-CID entry in the
/// array, regardless of how many of them there are.
const MAX_CID_WIDTH_ENTRIES: usize = 1_000_000;

/// Parses a CID `/W` array: `c [w1 w2 ...]` gives consecutive widths from
/// CID `c`; `c1 c2 w` gives every CID in `c1..=c2` width `w`. The TOTAL
/// number of entries inserted across the whole array is capped at
/// `MAX_CID_WIDTH_ENTRIES` (not merely each range), so a hostile array with
/// many ranges can't allocate unbounded memory; a font that hits the cap is
/// malformed, so parsing simply stops and keeps whatever was parsed so far.
fn parse_cid_width_array(doc: &Document, items: &[Object], map: &mut HashMap<u32, f32>) {
    let resolved: Vec<Object> = items
        .iter()
        .map(|o| doc.resolve(o).unwrap_or(Object::Null))
        .collect();
    let mut i = 0;
    while i < resolved.len() {
        if map.len() >= MAX_CID_WIDTH_ENTRIES {
            break;
        }
        let Some(first) = resolved[i].as_int() else {
            i += 1;
            continue;
        };
        let first = first.max(0) as u32;
        match resolved.get(i + 1) {
            Some(Object::Array(list)) => {
                for (j, item) in list.iter().enumerate() {
                    if map.len() >= MAX_CID_WIDTH_ENTRIES {
                        break;
                    }
                    let Some(code) = first.checked_add(j as u32) else {
                        break; // start CID so large the CIDs overflow u32
                    };
                    if let Some(w) = doc.resolve(item).ok().and_then(|o| o.as_f64()) {
                        map.insert(code, w as f32);
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
                        if map.len() >= MAX_CID_WIDTH_ENTRIES {
                            break;
                        }
                        map.insert(c, w as f32);
                    }
                }
                i += 3;
            }
            _ => i += 1,
        }
    }
}

/// Loads a `CIDFontType0` descendant (embedded CFF). Codes are assumed two
/// bytes (`Identity-H`/`Identity-V`, the embedded-subset norm) and are CIDs;
/// the CID-to-GID mapping comes from the CFF's own charset (`cid_to_gid`).
/// `/CIDToGIDMap` is a `CIDFontType2`-only key (it maps into a `glyf`
/// program); a `CIDFontType0` descendant is not expected to carry one, so it
/// is not consulted here.
fn load_cff_cid(doc: &Document, cid: &Dict) -> Option<GlyphFont> {
    let descriptor = resolve_dict(doc, cid.get("FontDescriptor")?)?;
    let program = stream_bytes(doc, descriptor.get("FontFile3")?)?;
    let cff = CffFont::parse(program)?;
    let cid_to_gid = cff.cid_to_gid();
    let widths = cid_widths(doc, cid);
    Some(GlyphFont {
        outlines: Outlines::Cff(cff),
        kind: GlyphKind::Cid(Some(cid_to_gid)),
        widths,
        afm_widths: HashMap::new(),
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

    use crate::cff::tests::{build_box_glyph_fixture, build_box_glyph_fixture_cid};
    use crate::truetype::tests::build_font;
    use crate::type1::tests::{build_type1_box_fixture, build_type1_box_fixture_standard_encoding};
    use crate::{GlyphPainting, Pixmap, RenderOptions, SubstituteSource};

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

    /// True iff a dark pixel lands at (55,115) — the known interior point of
    /// the rectangle-glyph fixtures below (both `truetype::tests::build_font`
    /// and `cff::tests::build_box_glyph_fixture[_cid]` trace the same
    /// (100,0)-(600,700) box in 1000-upm units, shown at 100pt from origin
    /// (20,50) on a 200x200 page).
    fn dark_pixel_at(pix: &Pixmap, x: u32, y: u32) -> bool {
        let o = ((y * pix.width + x) * 4) as usize;
        pix.data[o] < 128 && pix.data[o + 1] < 128 && pix.data[o + 2] < 128
    }

    /// The rectangle glyph (gid 1) is painted iff a dark pixel lands at (55,115),
    /// matching the geometry asserted in `truetype`'s render tests.
    fn glyph_painted(bytes: Vec<u8>) -> bool {
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let pix = crate::render_page(&doc, &page, 1.0).expect("render");
        dark_pixel_at(&pix, 55, 115)
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

    /// Builds a one-page PDF like `simple_font_doc`, but the font is a simple
    /// `/Type1` font whose `FontDescriptor` carries an embedded CFF program
    /// via `FontFile3` (rather than a `FontFile2` TrueType program).
    fn simple_cff_font_doc(encoding: &str, content: &[u8]) -> Vec<u8> {
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
                "<< /Type /Font /Subtype /Type1 /BaseFont /X \
                 /FontDescriptor 6 0 R {encoding} >>"
            ),
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile3 7 0 R >>",
        );
        b.stream(7, "", &build_box_glyph_fixture("theboxglyphname"));
        b.build(1)
    }

    /// Renders page 0 of `bytes` at the given glyph-painting tier.
    fn render_at_tier(bytes: &[u8], tier: GlyphPainting) -> Pixmap {
        let doc = Document::load(bytes.to_vec()).expect("load");
        let page = doc.page(0).expect("page");
        let opts = RenderOptions {
            glyph_painting: tier,
            ..Default::default()
        };
        crate::render_page_with_options(&doc, &page, 1.0, &opts).expect("render")
    }

    #[test]
    fn cff_simple_font_paints_at_all_embedded_and_full_not_embedded_truetype_only() {
        // Code 0x80's /Differences name resolves through the CFF's own
        // charset (no post table, no cmap -- CFF has neither).
        let bytes = simple_cff_font_doc(
            "/Encoding << /Differences [128 /theboxglyphname] >>",
            b"BT /F0 100 Tf 20 50 Td <80> Tj ET",
        );

        for tier in [GlyphPainting::AllEmbedded, GlyphPainting::Full] {
            let pix = render_at_tier(&bytes, tier);
            assert!(
                dark_pixel_at(&pix, 55, 115),
                "embedded CFF glyph should paint at tier {tier:?}"
            );
        }

        // The tier gate's whole point: at `EmbeddedTrueTypeOnly`, embedded
        // CFF must NOT paint, and the page stays blank.
        let pix = render_at_tier(&bytes, GlyphPainting::EmbeddedTrueTypeOnly);
        assert!(
            !dark_pixel_at(&pix, 55, 115),
            "embedded CFF must not paint at EmbeddedTrueTypeOnly (tier gate)"
        );
    }

    /// Builds a one-page PDF showing CID 5 (mapped to the box glyph via the
    /// CFF charset) of a `/Type0`/`CIDFontType0` font carrying an embedded
    /// CFF program.
    fn cid_cff_font_doc() -> Vec<u8> {
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <0005> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
             /DescendantFonts [6 0 R] >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /X \
             /FontDescriptor 7 0 R >>",
        );
        b.object(
            7,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile3 8 0 R >>",
        );
        b.stream(8, "", &build_box_glyph_fixture_cid(5));
        b.build(1)
    }

    #[test]
    fn cff_cid_font_paints_at_all_embedded_not_embedded_truetype_only() {
        let bytes = cid_cff_font_doc();

        let pix = render_at_tier(&bytes, GlyphPainting::AllEmbedded);
        assert!(
            dark_pixel_at(&pix, 55, 115),
            "embedded CIDFontType0 (CFF) glyph should paint at AllEmbedded"
        );

        let pix = render_at_tier(&bytes, GlyphPainting::EmbeddedTrueTypeOnly);
        assert!(
            !dark_pixel_at(&pix, 55, 115),
            "embedded CIDFontType0 (CFF) must not paint at EmbeddedTrueTypeOnly (tier gate)"
        );
    }

    // --- Task 4: advance from the PDF's declared /Widths (/W + /DW) ---------
    //
    // `build_font`'s synthetic sfnt carries no `hhea`/`hmtx` tables, so the
    // TrueType program's own advance (and CFF's, which has no advance table
    // at all) is always 0 for these fixtures. That makes a `/Widths` (or
    // `/W`+`/DW`) entry of 800 (out of 1000 units-per-em) an unambiguous
    // signal: the second glyph's painted origin only lands at the
    // `/Widths`-implied x (20 + 80 + 35 = 135) if `advance` reads the PDF
    // width instead of the (zero) program advance.

    #[test]
    fn simple_truetype_widths_advance_governs_second_glyph_origin() {
        // Two 'A's (code 0x41, gid 1): /FirstChar 65 /Widths [800] declares
        // an 800/1000-em advance for code 65, far from the program's 0.
        let bytes = simple_font_doc(
            "/Encoding /WinAnsiEncoding /FirstChar 65 /Widths [800]",
            b"BT /F0 100 Tf 20 50 Td <4141> Tj ET",
        );
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let pix = crate::render_page(&doc, &page, 1.0).expect("render");
        assert!(
            dark_pixel_at(&pix, 55, 115),
            "first glyph paints at the usual (55,115)"
        );
        assert!(
            dark_pixel_at(&pix, 135, 115),
            "second glyph must paint at the /Widths-implied origin (135,115), \
             not stacked on the first glyph as the program's 0 advance would give"
        );
    }

    #[test]
    fn simple_cff_widths_advance_governs_second_glyph_origin() {
        // Same idea for CFF, whose program advance is unconditionally 0
        // (no per-glyph advance table is parsed for CFF outlines).
        let bytes = simple_cff_font_doc(
            "/Encoding << /Differences [128 /theboxglyphname] >> \
             /FirstChar 128 /Widths [800]",
            b"BT /F0 100 Tf 20 50 Td <8080> Tj ET",
        );
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let pix = crate::render_page(&doc, &page, 1.0).expect("render");
        assert!(
            dark_pixel_at(&pix, 55, 115),
            "first glyph paints at the usual (55,115)"
        );
        assert!(
            dark_pixel_at(&pix, 135, 115),
            "second glyph must paint at the /Widths-implied origin (135,115), \
             not stacked on the first glyph as the program's 0 advance would give"
        );
    }

    #[test]
    fn type0_truetype_w_dw_advance_governs_second_glyph_origin() {
        // Two CID-1 codes (identity CID-to-GID, no /CIDToGIDMap): the
        // descendant's /W declares CID 1's advance as 800/1000 em, and /DW
        // covers everything else -- both far from the program's 0.
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <00010001> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
             /DescendantFonts [6 0 R] >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X \
             /FontDescriptor 7 0 R /DW 1000 /W [1 [800]] >>",
        );
        b.object(
            7,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile2 8 0 R >>",
        );
        b.stream(8, "", &build_font());
        let bytes = b.build(1);

        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let pix = crate::render_page(&doc, &page, 1.0).expect("render");
        assert!(
            dark_pixel_at(&pix, 55, 115),
            "first glyph paints at the usual (55,115)"
        );
        assert!(
            dark_pixel_at(&pix, 135, 115),
            "second glyph must paint at the /W-implied origin (135,115), not \
             stacked on the first glyph as the program's 0 advance would give"
        );
    }

    #[test]
    fn cid_width_array_many_ranges_capped_does_not_oom() {
        // Adversarial-input guard: each `c1 c2 w` RANGE is already capped at
        // 65536 entries on its own, but nothing previously capped the NUMBER
        // of ranges. A /W array of many small, non-overlapping ranges (as
        // built here, all decimal literals -- no hex/binary blobs) could
        // expand to hundreds of millions of HashMap entries without the
        // aggregate `MAX_CID_WIDTH_ENTRIES` cap. 16 back-to-back
        // maximally-sized (65536-entry) ranges declare 1,048,576 entries in
        // total, which exceeds the 1,000,000 cap partway through the 16th
        // range, so this exercises the cap actually kicking in mid-array.
        let mut w_array = String::new();
        for k in 0u32..16 {
            let start = k * 65536;
            let end = start + 65535;
            let width = if k == 0 { 800 } else { 500 };
            w_array.push_str(&format!("{start} {end} {width} "));
        }

        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <00010001> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
             /DescendantFonts [6 0 R] >>",
        );
        b.object(
            6,
            &format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X \
                 /FontDescriptor 7 0 R /DW 1000 /W [{w_array}] >>"
            ),
        );
        b.object(
            7,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile2 8 0 R >>",
        );
        b.stream(8, "", &build_font());
        let bytes = b.build(1);

        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");

        let started = std::time::Instant::now();
        let pix = crate::render_page(&doc, &page, 1.0).expect("render");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "capped /W parse must complete quickly, not hang expanding \
             ~1M+ range entries unbounded"
        );

        assert!(
            dark_pixel_at(&pix, 55, 115),
            "first glyph paints at the usual (55,115)"
        );
        assert!(
            dark_pixel_at(&pix, 135, 115),
            "CID 1's width (800, declared by the first range, well within \
             the cap) must still govern the second glyph's origin -- the cap \
             degrades the tail of a hostile array, not the CIDs that fit"
        );
    }

    // --- Task 4: embedded Type1 (`FontFile`) simple fonts -------------------

    /// Builds a one-page PDF like `simple_cff_font_doc`, but the font is a
    /// simple `/Type1` font whose `FontDescriptor` carries a raw Type1
    /// charstring program via `FontFile` (rather than a CFF `FontFile3`
    /// program).
    fn simple_type1_font_doc(encoding: &str, content: &[u8]) -> Vec<u8> {
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
                "<< /Type /Font /Subtype /Type1 /BaseFont /X \
                 /FontDescriptor 6 0 R {encoding} >>"
            ),
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile 7 0 R >>",
        );
        b.stream(7, "", &build_type1_box_fixture("theboxglyphname"));
        b.build(1)
    }

    #[test]
    fn type1_simple_font_paints_at_all_embedded_not_embedded_truetype_only() {
        let bytes = simple_type1_font_doc(
            "/Encoding << /Differences [128 /theboxglyphname] >>",
            b"BT /F0 100 Tf 20 50 Td <80> Tj ET",
        );
        for tier in [GlyphPainting::AllEmbedded, GlyphPainting::Full] {
            let pix = render_at_tier(&bytes, tier);
            assert!(
                dark_pixel_at(&pix, 55, 115),
                "embedded Type1 glyph should paint at tier {tier:?}"
            );
        }
        let pix = render_at_tier(&bytes, GlyphPainting::EmbeddedTrueTypeOnly);
        assert!(
            !dark_pixel_at(&pix, 55, 115),
            "embedded Type1 must not paint at EmbeddedTrueTypeOnly (tier gate)"
        );
    }

    #[test]
    fn type1_builtin_encoding_paints_without_pdf_encoding() {
        // build_type1_box_fixture already sets a built-in /Encoding mapping
        // code 128 -> theboxglyphname; the PDF font dict omits /Encoding
        // entirely.
        let bytes = simple_type1_font_doc("", b"BT /F0 100 Tf 20 50 Td <80> Tj ET");
        let pix = render_at_tier(&bytes, GlyphPainting::AllEmbedded);
        assert!(
            dark_pixel_at(&pix, 55, 115),
            "Type1 built-in /Encoding should map code 128 with no PDF /Encoding"
        );
    }

    #[test]
    fn type1_builtin_standard_encoding_token_paints_without_pdf_encoding() {
        // The FontFile's built-in /Encoding is the bare `StandardEncoding`
        // token (not a `dup <code> /<name> put` array) mapping code 65 to a
        // glyph literally named "A"; the PDF font dict has NO /Encoding key
        // at all. Before this fix, `parse_encoding` left `builtin_encoding`
        // entirely empty for the bare-token form, so every code->GID tier
        // (Differences, base encoding, built-in encoding) came up empty and
        // the glyph painted nothing.
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <41> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /X /FontDescriptor 6 0 R >>",
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile 7 0 R >>",
        );
        b.stream(7, "", &build_type1_box_fixture_standard_encoding());
        let bytes = b.build(1);

        let pix = render_at_tier(&bytes, GlyphPainting::AllEmbedded);
        assert!(
            dark_pixel_at(&pix, 55, 115),
            "built-in StandardEncoding token should map code 65 ('A') with no \
             PDF /Encoding at all"
        );
    }

    #[test]
    fn type1_widths_advance_governs_second_glyph_origin() {
        // Two 0x80 codes: /FirstChar 128 /Widths [800] declares an 800/1000-em
        // advance for code 128 -- Type1's program advance is always 0 here
        // (see GlyphFont::advance's doc comment), so only the PDF /Widths
        // entry can put the second glyph at (135,115).
        let bytes = simple_type1_font_doc(
            "/Encoding << /Differences [128 /theboxglyphname] >> \
             /FirstChar 128 /Widths [800]",
            b"BT /F0 100 Tf 20 50 Td <8080> Tj ET",
        );
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let pix = crate::render_page(&doc, &page, 1.0).expect("render");
        assert!(dark_pixel_at(&pix, 55, 115), "first glyph at (55,115)");
        assert!(
            dark_pixel_at(&pix, 135, 115),
            "second glyph at the /Widths-implied (135,115)"
        );
    }

    #[test]
    fn type1_malformed_fontfile_degrades_to_blank() {
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <80> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /X /FontDescriptor 6 0 R \
             /Encoding << /Differences [128 /theboxglyphname] >> >>",
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile 7 0 R >>",
        );
        b.stream(7, "", b"not a real type1 program");
        let bytes = b.build(1);
        let pix = render_at_tier(&bytes, GlyphPainting::AllEmbedded);
        assert!(
            !dark_pixel_at(&pix, 55, 115),
            "malformed FontFile paints nothing, no panic"
        );
    }

    // --- Task 3: Full-tier substitution for non-embedded fonts --------------
    //
    // `build_font`'s synthetic cmap (a single format-4 segment covering only
    // code point 0x41) maps 'A' (0x41) to gid 1, the box glyph, and every
    // other code point to gid 0 (`.notdef`) -- see
    // `truetype::tests::maps_char_to_glyph_and_reads_outline`. Reusing
    // `build_font` as the SUBSTITUTE face below means only 'A' ever paints;
    // every other code (in particular a `/Differences`-mapped `/space`) is
    // expected to stay unpainted, exactly like a real space would.

    /// Builds a one-page PDF showing `content` with a simple, NON-embedded
    /// font (`/BaseFont /{base}`, no `FontDescriptor`/`FontFile*` at all) and
    /// the given `/Encoding` entry -- the `Full`-tier substitution loader's
    /// input.
    fn non_embedded_font_doc(base: &str, encoding: &str, content: &[u8]) -> Vec<u8> {
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
            &format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} {encoding} >>"),
        );
        b.build(1)
    }

    /// Writes `bytes` to `basename` inside a freshly created temp directory
    /// (e.g. `"Arimo[wght].ttf"`, matching `substitute::face_filename`'s
    /// output) and returns the directory, ready to hand to `SubstituteSource::
    /// Dir` / `DirProvider`.
    ///
    /// The directory name is derived from the PID *and* a process-wide
    /// monotonic counter, not a timestamp: two calls racing on separate
    /// threads (as happens under the default parallel test runner) can land
    /// in the same clock tick and collide on a timestamp-only name, which
    /// then causes one test's `remove_dir_all` cleanup to race another
    /// test's still-in-flight read of the (shared) directory -- an
    /// intermittent missing/half-deleted face file. The atomic counter makes
    /// every call's directory unique within this process, so no two tests
    /// ever share one, regardless of thread interleaving.
    fn write_temp_face(basename: &str, bytes: &[u8]) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pdfboss-glyph-substitute-test-{}-{}",
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join(basename), bytes).expect("write fixture face");
        dir
    }

    /// Renders page 0 of `bytes` with the given `RenderOptions` in full --
    /// unlike `render_at_tier`, which only varies the tier, this also varies
    /// `substitutes`, needed to exercise `Full`-tier substitution.
    fn render_with(bytes: &[u8], opts: RenderOptions) -> Pixmap {
        let doc = Document::load(bytes.to_vec()).expect("load");
        let page = doc.page(0).expect("page");
        crate::render_page_with_options(&doc, &page, 1.0, &opts).expect("render")
    }

    #[test]
    fn non_embedded_helvetica_paints_at_full_via_substitute() {
        // /Type1 /Helvetica, no FontFile* at all (non-embedded),
        // WinAnsiEncoding, showing 'A' (0x41) -- which the substitute face's
        // cmap maps to the box glyph.
        let bytes = non_embedded_font_doc(
            "Helvetica",
            "/Encoding /WinAnsiEncoding",
            b"BT /F0 100 Tf 20 50 Td <41> Tj ET",
        );
        let dir = write_temp_face("Arimo[wght].ttf", &build_font());

        // Full + a Dir provider: substitution kicks in, the glyph paints.
        let pix = render_with(
            &bytes,
            RenderOptions {
                glyph_painting: GlyphPainting::Full,
                substitutes: SubstituteSource::Dir(dir.clone()),
            },
        );
        assert!(
            dark_pixel_at(&pix, 55, 115),
            "non-embedded Helvetica should paint via Full-tier substitution"
        );

        // Full with SubstituteSource::None: no provider, so `Full` behaves
        // like `AllEmbedded` -- the non-embedded font stays blank.
        let pix = render_with(
            &bytes,
            RenderOptions {
                glyph_painting: GlyphPainting::Full,
                substitutes: SubstituteSource::None,
            },
        );
        assert!(
            !dark_pixel_at(&pix, 55, 115),
            "Full with SubstituteSource::None must not paint (no provider)"
        );

        // AllEmbedded with a provider present: substitution is Full-only, so
        // a non-embedded font still stays blank at any lower tier.
        let pix = render_with(
            &bytes,
            RenderOptions {
                glyph_painting: GlyphPainting::AllEmbedded,
                substitutes: SubstituteSource::Dir(dir.clone()),
            },
        );
        assert!(
            !dark_pixel_at(&pix, 55, 115),
            "AllEmbedded must not substitute even with a provider present (tier gate)"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_embedded_helvetica_uses_afm_width_when_widths_absent() {
        // /Helvetica, NO /Widths, showing two codes: 0x80 (/Differences
        // "space", no /BaseEncoding so StandardEncoding governs) then 0x41
        // ('A', via the base encoding). Helvetica's AFM "space" width is
        // 278/1000 em; at 100pt that's a 27.8pt advance, so 'A' should paint
        // at x = 20 + 27.8 + 35 (the box glyph's interior offset) = 82.8 --
        // proving AFM (not the substitute hmtx, which would give 0 here,
        // since build_font carries no hhea/hmtx) governs when /Widths is
        // absent. Code 0x80 itself resolves to no glyph in the substitute's
        // cmap (glyph_to_unicode("space") -> U+0020, uncovered by
        // `build_font`'s cmap), so it paints nothing -- exactly like a real
        // space.
        let bytes = non_embedded_font_doc(
            "Helvetica",
            "/Encoding << /Differences [128 /space] >>",
            b"BT /F0 100 Tf 20 50 Td <8041> Tj ET",
        );
        let dir = write_temp_face("Arimo[wght].ttf", &build_font());

        let pix = render_with(
            &bytes,
            RenderOptions {
                glyph_painting: GlyphPainting::Full,
                substitutes: SubstituteSource::Dir(dir.clone()),
            },
        );
        assert!(
            !dark_pixel_at(&pix, 55, 115),
            "the /space code itself should paint nothing"
        );
        assert!(
            dark_pixel_at(&pix, 83, 115),
            "'A' should land at the AFM-implied origin (20 + 27.8 + 35 ~= 83), \
             not stacked on the first glyph (0 advance) or elsewhere"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_embedded_helvetica_widths_still_wins_over_afm() {
        // Same as above, but an explicit /Widths [800] for code 128 (AFM
        // would say 278) must still govern: 'A' lands at the classic
        // /Widths-implied (135, 115), not the AFM-implied (~83, 115).
        let bytes = non_embedded_font_doc(
            "Helvetica",
            "/Encoding << /Differences [128 /space] >> /FirstChar 128 /Widths [800]",
            b"BT /F0 100 Tf 20 50 Td <8041> Tj ET",
        );
        let dir = write_temp_face("Arimo[wght].ttf", &build_font());

        let pix = render_with(
            &bytes,
            RenderOptions {
                glyph_painting: GlyphPainting::Full,
                substitutes: SubstituteSource::Dir(dir.clone()),
            },
        );
        assert!(
            !dark_pixel_at(&pix, 83, 115),
            "the AFM-implied origin must lose to /Widths"
        );
        assert!(
            dark_pixel_at(&pix, 135, 115),
            "'A' should land at the /Widths-implied origin (20 + 80 + 35 = 135)"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Task 5: real bundled-font integration (feature-gated) --------------
    //
    // The tests above all substitute the synthetic `build_font` box glyph via
    // a `Dir` provider, so they can assert one fixed pixel. These two instead
    // exercise the actual compiled-in `BuiltinProvider` (the real OFL Croscore
    // TrueType programs, gated behind the `substitute-fonts` feature): a real
    // Tinos 'A' has different coverage than the synthetic box, so painting is
    // asserted with a lenient, page-wide ink scan rather than one coordinate.

    /// Whether any pixel in `pix` is meaningfully darker than the white
    /// background -- a coverage-shape-agnostic ink check, unlike
    /// `dark_pixel_at`'s single hard-coded coordinate (tuned to the synthetic
    /// rectangle-glyph fixtures). A real substitute face's stems, serifs and
    /// anti-aliased edges land at different pixels than the synthetic box, so
    /// asserting real ink needs a scan across the whole page instead.
    #[cfg(feature = "substitute-fonts")]
    fn has_dark_ink(pix: &Pixmap) -> bool {
        pix.data
            .chunks_exact(4)
            .any(|p| p[0] < 200 && p[1] < 200 && p[2] < 200)
    }

    /// `/Type1 /Times-Roman`, no `FontFile*` (non-embedded), `WinAnsiEncoding`,
    /// showing 'A' -- rendered at `Full` with the real compiled-in
    /// `BuiltinProvider`, which maps a Times-family request to the bundled
    /// Tinos-Regular face (`substitute::face_filename`). Proves the feature's
    /// actual font bytes parse and paint real ink end to end, not just that
    /// the request-derivation and Dir-provider plumbing work.
    #[cfg(feature = "substitute-fonts")]
    #[test]
    fn non_embedded_times_paints_with_builtin_at_full() {
        let bytes = non_embedded_font_doc(
            "Times-Roman",
            "/Encoding /WinAnsiEncoding",
            b"BT /F0 100 Tf 20 50 Td <41> Tj ET",
        );
        let pix = render_with(
            &bytes,
            RenderOptions {
                glyph_painting: GlyphPainting::Full,
                substitutes: SubstituteSource::Builtin,
            },
        );
        assert!(
            has_dark_ink(&pix),
            "non-embedded Times-Roman should paint a real 'A' via the builtin Tinos substitute"
        );
    }

    /// `/Type1 /Symbol` has no license-clean substitute in v1 --
    /// `FaceRequest::from_font_dict` returns `None` for it -- so even at
    /// `Full` with the real builtin provider available, Symbol text must
    /// stay unpainted rather than borrowing an unrelated face's glyphs.
    #[cfg(feature = "substitute-fonts")]
    #[test]
    fn symbol_stays_unpainted_at_full() {
        let bytes = non_embedded_font_doc(
            "Symbol",
            "/Encoding /WinAnsiEncoding",
            b"BT /F0 100 Tf 20 50 Td <41> Tj ET",
        );
        let pix = render_with(
            &bytes,
            RenderOptions {
                glyph_painting: GlyphPainting::Full,
                substitutes: SubstituteSource::Builtin,
            },
        );
        assert!(
            !has_dark_ink(&pix),
            "Symbol has no v1 substitute; Full must leave it unpainted, not fall back to a wrong face"
        );
    }
}
