//! Font loading from a page's `/Font` resource dictionary: simple fonts
//! (byte codes, `/Encoding` + `/Differences`, `/Widths`) and Type0/CID
//! fonts (`/Encoding` CMap code splitting and code-to-CID, `/ToUnicode`
//! plus the collection's own CID-to-Unicode, descendant `/W` + `/DW` and
//! the vertical `/W2` + `/DW2`).

use crate::cmap::ToUnicode;
use crate::sfnt;
use pdfboss_core::cmap::{cid_to_unicode, type0_encoding, CidCmap, CidToUnicode};
use pdfboss_core::{decoded_stream_data_with, AsyncObjectSource, Dict, FastMap, Object};
use pdfboss_encoding as encodings;
use std::sync::Arc;

/// One character code split out of a show string: its value and how many
/// bytes it occupied (two codes with equal values but different widths are
/// different codes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharCode {
    pub code: u32,
    pub len: u8,
}

/// One show string's codes, yielded in order; built by [`Font::codes_in`].
pub(crate) struct Codes<'f> {
    bytes: &'f [u8],
    pos: usize,
    split: Split<'f>,
}

/// How the active font splits a show string into codes.
#[derive(Clone, Copy)]
enum Split<'f> {
    /// One byte per code (simple fonts).
    Single,
    /// The `/Encoding` CMap's codespaces (Type0 fonts with one).
    Codespaces(&'f CidCmap),
    /// Two bytes per code, a trailing odd byte its own code.
    Pairs,
}

impl Iterator for Codes<'_> {
    type Item = CharCode;

    fn next(&mut self) -> Option<CharCode> {
        let rest = &self.bytes[self.pos..];
        let first = *rest.first()?;
        let (code, len) = match self.split {
            Split::Single => (u32::from(first), 1),
            Split::Codespaces(cmap) => cmap.code_at(self.bytes, self.pos),
            Split::Pairs => match rest.get(1) {
                Some(&second) => (u32::from(u16::from_be_bytes([first, second])), 2),
                None => (u32::from(first), 1),
            },
        };
        self.pos += usize::from(len);
        Some(CharCode { code, len })
    }
}

/// The descriptor `/StemV` (glyph-space units) at which a face reads as
/// bold. Regular text faces report dominant vertical stems up to ~110 and
/// bold faces from ~140, so the cut sits in the gap between the clusters.
const BOLD_STEM_WIDTH: f64 = 120.0;

/// A loaded font: everything needed to decode show-string bytes to
/// Unicode and to advance the text position.
pub struct Font {
    /// True for simple (1-byte-code) fonts; false for Type0/CID fonts,
    /// whose code widths the `/Encoding` CMap decides (two bytes when
    /// there is none).
    pub simple: bool,
    /// `/ToUnicode` CMap when present — the highest-priority mapping,
    /// keyed by character code.
    to_unicode: Option<ToUnicode>,
    /// The Type0 `/Encoding` CMap: code splitting and code-to-CID.
    /// `None` for simple fonts and for the Identity assumption.
    cmap: Option<Arc<CidCmap>>,
    /// The character collection's CID-to-Unicode mapping (from
    /// `/CIDSystemInfo`), consulted for codes `/ToUnicode` misses.
    cid_to_unicode: Option<Arc<CidToUnicode>>,
    /// False when the Type0 `/Encoding` named a CMap that did not resolve
    /// (or was absent), so the code==CID reading is a guess rather than
    /// what the file states.
    pub encoding_known: bool,
    /// Writing mode 1: shown text advances downward by [`Font::vwidth`].
    pub vertical: bool,
    /// Per-code Unicode from the `/Encoding` base table plus
    /// `/Differences` (simple fonts only).
    encoding: Option<Box<[Option<Decoded>; 256]>>,
    /// Explicit widths in glyph-space units (1/1000 em), keyed by code for
    /// simple fonts and by CID for Type0 (`/W`).
    widths: FastMap<u32, f32>,
    /// Width used for codes without an explicit entry.
    default_width: f32,
    /// Vertical displacements (`/W2` w1, negative for downward), keyed by
    /// CID.
    vwidths: FastMap<u32, f32>,
    /// `/DW2`'s displacement, default -1000 (ISO 32000-1 §9.7.4.3).
    default_vwidth: f32,
    /// The code that triggers word spacing (single-byte code 32).
    space_code: Option<u32>,
    /// The font states no `/Encoding`, but its embedded program advertises a
    /// Microsoft `cmap` — so codes StandardEncoding leaves undefined are read
    /// as WinAnsiEncoding. See [`Font::decode_into`] for why that evidence is
    /// required rather than assumed.
    winansi_high_codes: bool,
    /// Style evidence for extraction: the FontDescriptor's Flags/FontWeight/
    /// ItalicAngle when present (ISO 32000-1 §9.8.1, Table 123), with
    /// BaseFont-name substrings as fallback.
    pub bold: bool,
    pub italic: bool,
    /// `/BaseFont` verbatim — subset prefix included — falling back to the
    /// FontDescriptor's `/FontName`; empty when the file states neither.
    pub base_name: String,
    /// Upper edge of the em box in per-mille units: `/Ascent`, else
    /// `/CapHeight`, else 800.
    pub ascent: f32,
    /// Lower edge of the em box in per-mille units, never positive:
    /// `/Descent`, else -200.
    pub descent: f32,
    /// FontDescriptor `/Flags` FixedPitch (ISO 32000-1 Table 123 bit 1).
    pub monospace: bool,
    /// FontDescriptor `/Flags` Serif (ISO 32000-1 Table 123 bit 2).
    pub serif: bool,
}

/// Everything [`Font::style`] reads in one descriptor pass: the font's
/// stated name, the weight/slant evidence, and the vertical metrics
/// extraction turns into span boxes.
struct Style {
    name: String,
    bold: bool,
    italic: bool,
    ascent: f32,
    descent: f32,
    monospace: bool,
    serif: bool,
}

/// One `/Encoding` table cell. Base-table entries and most `/Differences`
/// names decode to one scalar; AGL underscore ligatures (`f_i`, `T_h`) and
/// multi-group `uniXXXX` names decode to several.
#[derive(Clone)]
enum Decoded {
    Char(char),
    Text(Box<str>),
}

impl Decoded {
    /// Keeps the common single-scalar case on the allocation-free path.
    fn from_text(text: String) -> Decoded {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Decoded::Char(c),
            _ => Decoded::Text(text.into_boxed_str()),
        }
    }
}

/// Resolves `dict[key]`, treating resolution failures and `null` as absent.
///
/// Every function in this module borrows its source rather than owning it: they
/// are helpers reached from an entry point that already owns one, and the
/// `'static` question is settled at that boundary. See
/// `pdfboss_core::source`'s "Signing a shared algorithm".
async fn rv<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<Object> {
    let obj = dict.get(key)?;
    let resolved = src.resolve(obj).await.ok()?;
    (!resolved.is_null()).then_some(resolved)
}

impl Font {
    /// A last-resort font for missing or unloadable font resources:
    /// 1-byte codes, StandardEncoding fallback, width 500.
    pub fn fallback() -> Font {
        Font {
            simple: true,
            to_unicode: None,
            cmap: None,
            cid_to_unicode: None,
            encoding_known: true,
            vertical: false,
            encoding: None,
            widths: FastMap::default(),
            default_width: 500.0,
            vwidths: FastMap::default(),
            default_vwidth: -1000.0,
            space_code: Some(32),
            winansi_high_codes: false,
            bold: false,
            italic: false,
            base_name: String::new(),
            ascent: 800.0,
            descent: -200.0,
            monospace: false,
            serif: false,
        }
    }

    /// Loads a font from its (resolved) font dictionary. Lenient: anything
    /// missing or malformed degrades to defaults rather than failing.
    pub async fn load<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Font {
        let subtype = rv(src, dict, "Subtype").await;
        let is_type0 = subtype
            .as_ref()
            .and_then(|o| o.as_name())
            .is_some_and(|n| n.0 == "Type0");
        let to_unicode = Font::load_to_unicode(src, dict).await;
        if is_type0 {
            Font::load_type0(src, dict, to_unicode).await
        } else {
            Font::load_simple(src, dict, to_unicode).await
        }
    }

    /// Reads and parses `/ToUnicode`, treating an empty CMap as absent so that
    /// the lower-priority mappings still get their chance. Fetched through the
    /// checked fetch, not raw `stream_data`: a CMap whose trailing `/Filter`
    /// is an image codec holds a passthrough codestream, and token-scanning
    /// one yields an empty — or in principle a bogus — mapping.
    async fn load_to_unicode<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Option<ToUnicode> {
        let obj = rv(src, dict, "ToUnicode").await?;
        let data = decoded_stream_data_with(src, obj.as_stream()?).await.ok()?;
        let cmap = ToUnicode::parse(&data);
        (!cmap.is_empty()).then_some(cmap)
    }

    /// Splits a show-string into character codes as a list (test helper;
    /// lib code iterates [`Font::codes_in`] to avoid the allocation).
    #[cfg(test)]
    pub fn codes(&self, bytes: &[u8]) -> Vec<CharCode> {
        self.codes_in(bytes).collect()
    }

    /// Splits a show-string into character codes: one byte each for simple
    /// fonts, the `/Encoding` CMap's codespaces for Type0 fonts with one,
    /// and two bytes otherwise (a trailing odd byte becomes its own code).
    /// An iterator because the show loop reads each code once — the hottest
    /// operator on a text page never pays a per-string list allocation.
    pub(crate) fn codes_in<'f>(&'f self, bytes: &'f [u8]) -> Codes<'f> {
        let split = if self.simple {
            Split::Single
        } else if let Some(cmap) = self.cmap.as_deref() {
            Split::Codespaces(cmap)
        } else {
            Split::Pairs
        };
        Codes {
            bytes,
            pos: 0,
            split,
        }
    }

    /// The CID a code selects in the descendant font: through the
    /// `/Encoding` CMap when there is one (unmapped codes are CID 0, the
    /// notdef), the code itself otherwise (simple fonts and Identity).
    fn cid(&self, cc: CharCode) -> u32 {
        match &self.cmap {
            Some(cmap) => cmap.cid(cc.code, cc.len).unwrap_or(0),
            None => cc.code,
        }
    }

    /// Decodes one code to Unicode as a fresh `String` (test helper; lib code
    /// uses [`Font::decode_into`] to avoid the per-glyph allocation).
    #[cfg(test)]
    pub fn decode(&self, code: u32) -> String {
        let mut out = String::new();
        let len = if self.simple { 1 } else { 2 };
        self.decode_into(CharCode { code, len }, &mut out);
        out
    }

    /// Decodes one code to Unicode, appending to `out`. Priority:
    /// `/ToUnicode`, then the `/Encoding`-derived table, then StandardEncoding,
    /// then WinAnsiEncoding for the codes StandardEncoding leaves undefined
    /// (simple fonts), then U+FFFD. The common single-glyph paths push one
    /// `char` with no allocation; only a multi-unit `/ToUnicode` mapping copies
    /// a string.
    ///
    /// # Why WinAnsiEncoding needs evidence before it is consulted
    ///
    /// A simple font carrying neither `/ToUnicode` nor `/Encoding` leaves the
    /// text with no mapping the file states outright, and ISO 32000-1 9.6.6.4
    /// sends the reader to the font program's own built-in encoding. For the
    /// subset TrueType fonts that dominate real documents that route stops
    /// short of Unicode: the `cmap` maps a code to a *glyph index*, and with a
    /// version 3.0 `post` table carrying no glyph names, nothing in the font
    /// says which character that glyph draws.
    ///
    /// What the program does say is which platform it was built for, and that
    /// is the whole basis for the guess made here. A Microsoft `cmap` subtable
    /// means the producer indexed the subset by Windows code points, so the
    /// codes StandardEncoding leaves undefined — 0x80 to 0x9F, where Windows
    /// documents keep their curly quotes and dashes — are WinAnsiEncoding's.
    ///
    /// Absent that evidence the guess is not made, because measuring it against
    /// a 259-file corpus showed it is a coin flip: of the documents it changed,
    /// it read some degree signs and apostrophes correctly and turned a
    /// Macintosh-encoded bulletin's apostrophes into `Õ` and its em dashes into
    /// `Ñ`, and four documents' Symbol and dingbat fonts into thorns and
    /// slashed Os. Every one of those fonts was **non-embedded** — the file
    /// offered nothing to read — so requiring a font program is what separates
    /// the case with evidence from the case without it. U+FFFD is the worse
    /// answer only when something better is actually known.
    ///
    /// Even with the evidence this only ever fills gaps: it is reached solely
    /// for codes StandardEncoding leaves undefined, so no mapping that already
    /// resolved can change.
    ///
    /// # Why this is `#[inline]`
    ///
    /// This and the two accessors below run once per character code — tens of
    /// thousands of times per page of text — and converting font *loading* to
    /// `async fn` cost 4.9% on `extract_text_warm_500_lines` without adding any
    /// work to this path: that page loads exactly one font. The regression was
    /// codegen fallout, and asking for these three back explicitly recovers
    /// about 1.9% of it. Measured by interleaved A/B runs of prebuilt
    /// benchmark binaries, which is the only method this machine's drift permits.
    #[inline]
    pub fn decode_into(&self, cc: CharCode, out: &mut String) {
        let code = cc.code;
        if let Some(c) = self.to_unicode.as_ref() {
            if let Some(s) = c.lookup(code) {
                out.push_str(&s);
                return;
            }
        }
        if !self.simple {
            if let Some(c) = self.collection_unicode(cc) {
                out.push(c);
                return;
            }
        }
        if self.simple {
            if let Ok(byte) = u8::try_from(code) {
                match self.encoding.as_ref().map(|t| &t[byte as usize]) {
                    Some(Some(Decoded::Char(c))) => {
                        out.push(*c);
                        return;
                    }
                    Some(Some(Decoded::Text(s))) => {
                        out.push_str(s);
                        return;
                    }
                    _ => {}
                }
                let fallback = encodings::standard(byte).or_else(|| {
                    self.winansi_high_codes
                        .then(|| encodings::win_ansi(byte))
                        .flatten()
                });
                if let Some(c) = fallback {
                    out.push(c);
                    return;
                }
            }
        }
        out.push('\u{FFFD}');
    }

    /// The character collection's Unicode for the CID `cc` selects, when
    /// the collection is known. The deepest `usecmap` layer answers first:
    /// a vertical CMap only swaps in rotated-variant CIDs — presentation
    /// forms to the collection's Unicode mapping, when it knows them at
    /// all — while its horizontal base names the character itself.
    fn collection_unicode(&self, cc: CharCode) -> Option<char> {
        let inv = self.cid_to_unicode.as_ref()?;
        let Some(cmap) = &self.cmap else {
            return inv.lookup(cc.code); // Identity: the code is the CID
        };
        let mut layers = Vec::new();
        let mut layer = Some(cmap);
        while let Some(l) = layer {
            layers.push(l);
            layer = l.parent();
        }
        layers
            .iter()
            .rev()
            .find_map(|l| l.cid(cc.code, cc.len).and_then(|cid| inv.lookup(cid)))
    }

    /// Glyph-space width (1/1000 em) of `cc` — `/W` and `/DW` key on the
    /// CID, so the code maps first. `#[inline]` for the reason given on
    /// [`Font::decode_into`].
    #[inline]
    pub fn width(&self, cc: CharCode) -> f32 {
        self.widths
            .get(&self.cid(cc))
            .copied()
            .unwrap_or(self.default_width)
    }

    /// Glyph-space vertical displacement (1/1000 em, negative downward) of
    /// `cc`, from `/W2` keyed by CID, else `/DW2`.
    pub fn vwidth(&self, cc: CharCode) -> f32 {
        self.vwidths
            .get(&self.cid(cc))
            .copied()
            .unwrap_or(self.default_vwidth)
    }

    /// True when showing `cc` applies word spacing (`Tw`): the single-byte
    /// code 32 (ISO 32000-1 §9.3.3 — a 2-byte code with value 32 is not a
    /// space). `#[inline]` for the reason given on [`Font::decode_into`].
    #[inline]
    pub fn is_space(&self, cc: CharCode) -> bool {
        self.space_code == Some(cc.code) && cc.len == 1
    }

    /// Whether `/BaseFont` names a face whose glyphs are pictures rather than
    /// letters, for which no byte-to-character encoding means anything.
    ///
    /// The name is matched on its family part: a subset prefix (six capitals
    /// and a plus sign, ISO 32000-1 9.6.4) and any `,Bold` style suffix are
    /// stripped first, so `JOGDGG+Wingdings` and `Symbol,Italic` both match.
    async fn is_picture_font<S: AsyncObjectSource>(src: &S, dict: &Dict) -> bool {
        /// Families whose code points index a picture set. Anything absent
        /// from this list is treated as text, which is the safe direction:
        /// the worst case is a code that stays U+FFFD.
        const PICTURE_FAMILIES: [&str; 7] = [
            "symbol",
            "zapfdingbats",
            "dingbats",
            "wingdings",
            "wingdings2",
            "wingdings3",
            "webdings",
        ];
        let Some(name) = rv(src, dict, "BaseFont")
            .await
            .and_then(|o| o.as_name().map(|n| n.0.clone()))
        else {
            return false;
        };
        let family = name
            .split_once('+')
            .map_or(name.as_str(), |(prefix, rest)| {
                if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) {
                    rest
                } else {
                    name.as_str()
                }
            })
            .split(&[',', '-'][..])
            .next()
            .unwrap_or_default()
            .replace(' ', "")
            .to_ascii_lowercase();
        PICTURE_FAMILIES.contains(&family.as_str())
    }

    /// Bold/italic evidence for `dict` (whose `/BaseFont` is read directly):
    /// BaseFont name substrings first, then `descriptor_holder`'s own
    /// `/FontDescriptor` — the font's own dict for simple fonts, the
    /// already-resolved descendant dict for Type0 — refining the guess with
    /// `/Flags`, `/FontWeight`, and `/ItalicAngle` (ISO 32000-1 §9.8.1,
    /// Table 123).
    async fn style<S: AsyncObjectSource>(src: &S, dict: &Dict, descriptor_holder: &Dict) -> Style {
        let name = rv(src, dict, "BaseFont")
            .await
            .and_then(|o| o.as_name().map(|n| n.0.clone()))
            .unwrap_or_default();
        let mut style = Style {
            bold: name.contains("Bold"),
            italic: name.contains("Italic") || name.contains("Oblique"),
            name,
            ascent: 800.0,
            descent: -200.0,
            monospace: false,
            serif: false,
        };
        let Some(descriptor) = rv(src, descriptor_holder, "FontDescriptor")
            .await
            .and_then(|o| o.as_dict().cloned())
        else {
            return style;
        };
        if style.name.is_empty() {
            style.name = rv(src, &descriptor, "FontName")
                .await
                .and_then(|o| o.as_name().map(|n| n.0.clone()))
                .unwrap_or_default();
        }
        let ascent = match rv(src, &descriptor, "Ascent")
            .await
            .and_then(|o| o.as_f64())
        {
            Some(a) if a != 0.0 => Some(a),
            _ => rv(src, &descriptor, "CapHeight")
                .await
                .and_then(|o| o.as_f64())
                .filter(|c| *c != 0.0),
        };
        if let Some(a) = ascent {
            style.ascent = a as f32;
        }
        if let Some(d) = rv(src, &descriptor, "Descent")
            .await
            .and_then(|o| o.as_f64())
            .filter(|d| *d != 0.0)
        {
            // Stated as negative (ISO 32000-1 Table 122); some producers
            // write the magnitude.
            style.descent = -(d as f32).abs();
        }
        let mut bold = style.bold;
        let mut italic = style.italic;
        if let Some(flags) = rv(src, &descriptor, "Flags").await.and_then(|o| o.as_int()) {
            italic = italic || flags & (1 << 6) != 0; // Table 123 bit 7: Italic
            bold = bold || flags & (1 << 18) != 0; // Table 123 bit 19: ForceBold
            style.monospace = flags & 1 != 0; // Table 123 bit 1: FixedPitch
            style.serif = flags & 2 != 0; // Table 123 bit 2: Serif
        }
        if let Some(weight) = rv(src, &descriptor, "FontWeight")
            .await
            .and_then(|o| o.as_f64())
        {
            bold = bold || weight >= 600.0;
        }
        // Table 122: StemV is the thickness of the dominant vertical stems.
        // Text faces stay under ~110 glyph-space units and bold faces start
        // around 140, so a thick stem marks bold fonts whose descriptors
        // carry neither a weight nor a telling name (URW's -Medi faces).
        if let Some(stem) = rv(src, &descriptor, "StemV").await.and_then(|o| o.as_f64()) {
            bold = bold || stem >= BOLD_STEM_WIDTH;
        }
        if let Some(angle) = rv(src, &descriptor, "ItalicAngle")
            .await
            .and_then(|o| o.as_f64())
        {
            italic = italic || angle != 0.0;
        }
        style.bold = bold;
        style.italic = italic;
        style
    }

    /// Loads a Type1/TrueType/Type3 font: 1-byte codes, `/Encoding` base
    /// plus `/Differences`, widths from `/FirstChar` + `/Widths`.
    async fn load_simple<S: AsyncObjectSource>(
        src: &S,
        dict: &Dict,
        to_unicode: Option<ToUnicode>,
    ) -> Font {
        let encoding = Font::load_encoding(src, dict).await;

        let mut widths = FastMap::default();
        let first = rv(src, dict, "FirstChar")
            .await
            .and_then(|o| o.as_int())
            .unwrap_or(0)
            .max(0) as u32;
        if let Some(Object::Array(items)) = rv(src, dict, "Widths").await {
            for (i, item) in items.iter().enumerate() {
                let Some(code) = first.checked_add(i as u32) else {
                    break; // /FirstChar so large the codes overflow u32
                };
                if let Some(w) = src.resolve(item).await.ok().and_then(|o| o.as_f64()) {
                    widths.insert(code, w as f32);
                }
            }
        }
        let descriptor = rv(src, dict, "FontDescriptor")
            .await
            .and_then(|o| o.as_dict().cloned());
        let default_width = match &descriptor {
            Some(fd) => rv(src, fd, "MissingWidth")
                .await
                .and_then(|o| o.as_f64())
                .map_or(500.0, |w| w as f32),
            None => 500.0,
        };

        // Only a font that states no `/Encoding` has anything to gain here, and
        // only then is the embedded program worth inflating.
        let winansi_high_codes = encoding.is_none() && Font::built_for_windows(src, dict).await;
        let style = Font::style(src, dict, dict).await;

        Font {
            simple: true,
            to_unicode,
            cmap: None,
            cid_to_unicode: None,
            encoding_known: true,
            vertical: false,
            encoding,
            widths,
            default_width,
            vwidths: FastMap::default(),
            default_vwidth: -1000.0,
            space_code: Some(32),
            winansi_high_codes,
            bold: style.bold,
            italic: style.italic,
            base_name: style.name,
            ascent: style.ascent,
            descent: style.descent,
            monospace: style.monospace,
            serif: style.serif,
        }
    }

    /// Whether the font's embedded program advertises a Microsoft `cmap`,
    /// which is the evidence [`Font::decode_into`] requires before reading a
    /// code StandardEncoding leaves undefined as WinAnsiEncoding.
    ///
    /// A font with no program at all answers `false`: there is nothing to read,
    /// and the measurement recorded on `decode_into` is that guessing without
    /// it goes wrong about as often as it goes right. A program advertising
    /// only a Macintosh subtable answers `false` too — those are the documents
    /// the guess actively damaged.
    ///
    /// A picture font answers `false` by name, which deserves an explanation
    /// because naming fonts is exactly the sort of special-casing that usually
    /// signals a missing principle. Here there is no principle left to find. A
    /// Wingdings subset and a Times New Roman subset from the same producer are
    /// structurally identical: both symbolic, both `/FontFile2`, both carrying a
    /// Macintosh and a Microsoft subtable, and both mapping the private-use
    /// range 0xF020 to 0xF0FF rather than raw codes — that last was measured
    /// against a real pair of them, in the hope it would separate the two, and
    /// it does not. With no glyph names and no Unicode subtable, nothing inside
    /// either font says whether its glyphs are letters or pictures. The
    /// `/BaseFont` name is the only thing left that does.
    async fn built_for_windows<S: AsyncObjectSource>(src: &S, dict: &Dict) -> bool {
        if Font::is_picture_font(src, dict).await {
            return false;
        }
        let Some(descriptor) = rv(src, dict, "FontDescriptor")
            .await
            .and_then(|o| o.as_dict().cloned())
        else {
            return false;
        };
        // `/FontFile` is a Type 1 program, which is not an sfnt and carries no
        // `cmap`; only the two sfnt-bearing entries are worth reading.
        for key in ["FontFile2", "FontFile3"] {
            // Absent, not a stream, and undecodable all mean the same thing
            // here: this entry says nothing, try the next one.
            let Some(data) = Font::sfnt_program(src, &descriptor, key).await else {
                continue;
            };
            let platforms = sfnt::cmap_platforms(&data);
            if platforms.microsoft {
                return true;
            }
        }
        false
    }

    /// Decoded bytes of `descriptor[key]` when it is a readable stream.
    /// Fetched through the checked fetch, not raw `stream_data`: a font
    /// program whose trailing `/Filter` is an image codec holds a
    /// passthrough codestream, and reading one as an sfnt table directory
    /// would let arbitrary JPEG bytes decide the WinAnsi guess.
    async fn sfnt_program<S: AsyncObjectSource>(
        src: &S,
        descriptor: &Dict,
        key: &str,
    ) -> Option<Vec<u8>> {
        let obj = rv(src, descriptor, key).await?;
        decoded_stream_data_with(src, obj.as_stream()?).await.ok()
    }

    /// Builds the 256-entry Unicode table from `/Encoding`: a base table
    /// (named directly or via `/BaseEncoding`, default Standard) with
    /// `/Differences` glyph names applied on top.
    async fn load_encoding<S: AsyncObjectSource>(
        src: &S,
        dict: &Dict,
    ) -> Option<Box<[Option<Decoded>; 256]>> {
        let enc = rv(src, dict, "Encoding").await?;
        let base_name = match &enc {
            Object::Name(n) => Some(n.0.clone()),
            Object::Dict(d) => rv(src, d, "BaseEncoding")
                .await
                .and_then(|o| o.as_name().map(|n| n.0.clone())),
            _ => None,
        };
        let base: fn(u8) -> Option<char> = match base_name.as_deref() {
            Some("WinAnsiEncoding") => encodings::win_ansi,
            Some("MacRomanEncoding") => encodings::mac_roman,
            _ => encodings::standard,
        };
        let mut table: Box<[Option<Decoded>; 256]> = Box::new(std::array::from_fn(|_| None));
        for (code, slot) in table.iter_mut().enumerate() {
            *slot = base(code as u8).map(Decoded::Char);
        }
        if let Object::Dict(d) = &enc {
            if let Some(Object::Array(diffs)) = rv(src, d, "Differences").await {
                let mut code: u32 = 0;
                for item in &diffs {
                    match src.resolve(item).await.ok() {
                        Some(Object::Int(n)) => code = n.max(0) as u32,
                        Some(Object::Real(n)) => code = n.max(0.0) as u32,
                        Some(Object::Name(name)) => {
                            if code < 256 {
                                table[code as usize] =
                                    encodings::glyph_to_text(&name.0).map(Decoded::from_text);
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

    /// Loads a Type0/CID font: `/Encoding` decides code splitting and the
    /// code-to-CID mapping (Identity stays 2-byte code==CID; predefined
    /// names and embedded CMap streams resolve through `pdfboss_core::
    /// cmap`; anything unresolvable keeps the Identity guess, noted on
    /// `encoding_known`). Unicode comes from `/ToUnicode` first, then the
    /// `/CIDSystemInfo` collection's own mapping. Widths are the
    /// descendant's `/W` + `/DW` keyed by CID; vertical displacements its
    /// `/W2` + `/DW2`.
    async fn load_type0<S: AsyncObjectSource>(
        src: &S,
        dict: &Dict,
        to_unicode: Option<ToUnicode>,
    ) -> Font {
        let descendant = Font::load_descendant(src, dict).await;
        let encoding = type0_encoding(src, dict).await;

        let mut widths = FastMap::default();
        let mut default_width = 1000.0;
        let mut vwidths = FastMap::default();
        let mut default_vwidth = -1000.0;
        let mut ordering = None;
        if let Some(desc) = &descendant {
            if let Some(dw) = rv(src, desc, "DW").await.and_then(|o| o.as_f64()) {
                default_width = dw as f32;
            }
            if let Some(Object::Array(w)) = rv(src, desc, "W").await {
                Font::parse_cid_widths(src, &w, &mut widths).await;
            }
            if let Some(Object::Array(dw2)) = rv(src, desc, "DW2").await {
                if let Some(w1) = dw2.get(1).and_then(|o| o.as_f64()) {
                    default_vwidth = w1 as f32;
                }
            }
            if let Some(Object::Array(w2)) = rv(src, desc, "W2").await {
                Font::parse_cid_vwidths(src, &w2, &mut vwidths).await;
            }
            if let Some(info) = rv(src, desc, "CIDSystemInfo")
                .await
                .and_then(|o| o.as_dict().cloned())
            {
                ordering = rv(src, &info, "Ordering")
                    .await
                    .and_then(|o| o.as_str_bytes().map(|b| b.to_vec()));
            }
        }
        // The descendant already holds the /FontDescriptor for a Type0 font
        // (ISO 32000-1 §9.7.4); reuse the dict just resolved above rather
        // than resolving /DescendantFonts a second time.
        let descriptor_holder = descendant.as_ref().unwrap_or(dict);
        let style = Font::style(src, dict, descriptor_holder).await;

        let space_code = encoding
            .cmap
            .as_ref()
            .is_some_and(|c| c.single_byte(32))
            .then_some(32);
        let collection = ordering
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|o| cid_to_unicode(&o));
        Font {
            simple: false,
            to_unicode,
            cid_to_unicode: collection,
            encoding_known: encoding.known,
            vertical: encoding.vertical,
            cmap: encoding.cmap,
            encoding: None,
            widths,
            default_width,
            vwidths,
            default_vwidth,
            space_code,
            // Composite codes never reach the single-byte fallbacks.
            winansi_high_codes: false,
            bold: style.bold,
            italic: style.italic,
            base_name: style.name,
            ascent: style.ascent,
            descent: style.descent,
            monospace: style.monospace,
            serif: style.serif,
        }
    }

    /// The first entry of `/DescendantFonts`, which is where a Type0 font keeps
    /// its widths. ISO 32000-1 9.7.4 allows the array exactly one element.
    async fn load_descendant<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Option<Dict> {
        let obj = rv(src, dict, "DescendantFonts").await?;
        let first = obj.as_array()?.first()?.clone();
        src.resolve(&first).await.ok()?.as_dict().cloned()
    }

    /// Parses a CID `/W` array: `c [w1 w2 …]` gives consecutive widths
    /// from CID `c`; `c1 c2 w` gives every CID in `c1..=c2` width `w`
    /// (ranges capped at 65536 entries).
    async fn parse_cid_widths<S: AsyncObjectSource>(
        src: &S,
        items: &[Object],
        widths: &mut FastMap<u32, f32>,
    ) {
        let mut resolved: Vec<Object> = Vec::with_capacity(items.len());
        for item in items {
            resolved.push(src.resolve(item).await.unwrap_or(Object::Null));
        }
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
                        if let Some(w) = src.resolve(item).await.ok().and_then(|o| o.as_f64()) {
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

    /// Parses a CID `/W2` array (ISO 32000-1 §9.7.4.3): `c [w1 vx vy …]`
    /// gives per-CID triples from CID `c`; `c1 c2 w1 vx vy` gives every CID
    /// in `c1..=c2` the same metrics (ranges capped at 65536 entries). Only
    /// the vertical displacement `w1` matters to extraction; the position
    /// vector shifts where the glyph paints, not where the text goes next.
    async fn parse_cid_vwidths<S: AsyncObjectSource>(
        src: &S,
        items: &[Object],
        vwidths: &mut FastMap<u32, f32>,
    ) {
        let mut resolved: Vec<Object> = Vec::with_capacity(items.len());
        for item in items {
            resolved.push(src.resolve(item).await.unwrap_or(Object::Null));
        }
        let mut i = 0;
        while i < resolved.len() {
            let Some(first) = resolved[i].as_int() else {
                i += 1;
                continue;
            };
            let first = first.max(0) as u32;
            match resolved.get(i + 1) {
                Some(Object::Array(list)) => {
                    for (j, triple) in list.chunks(3).enumerate() {
                        let Some(cid) = first.checked_add(j as u32) else {
                            break; // start CID so large the CIDs overflow u32
                        };
                        if let Some(w1) =
                            src.resolve(&triple[0]).await.ok().and_then(|o| o.as_f64())
                        {
                            vwidths.insert(cid, w1 as f32);
                        }
                    }
                    i += 2;
                }
                Some(other) if other.as_f64().is_some() => {
                    let last = other.as_int().unwrap_or(first as i64).max(0) as u32;
                    let w1 = resolved.get(i + 2).and_then(|o| o.as_f64());
                    if let Some(w1) = w1 {
                        let end = last.min(first.saturating_add(65535));
                        for c in first..=end.max(first) {
                            vwidths.insert(c, w1 as f32);
                        }
                    }
                    i += 5;
                }
                _ => i += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{block_on, Document, Immediate, ObjRef};
    use pdfboss_testkit::PdfBuilder;

    fn one(code: u32) -> CharCode {
        CharCode { code, len: 1 }
    }

    fn two(code: u32) -> CharCode {
        CharCode { code, len: 2 }
    }

    /// Builds a document whose object 5 is `font_body`. `extra` adds stream
    /// objects (ToUnicode CMaps, font programs) and `objects` adds plain
    /// ones (font descriptors); either can reference or be referenced by it.
    fn font_from(font_body: &str, extra: &[(u32, &[u8])], objects: &[(u32, &str)]) -> Font {
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
        for &(num, body) in objects {
            b.object(num, body);
        }
        let doc = Document::load(b.build(1)).unwrap();
        let obj = doc.get(ObjRef { num: 5, gen: 0 }).unwrap();
        // Loading is the shared asynchronous implementation; a synchronous
        // caller reaches it the same way the public entry points do. Every
        // assertion below is unaffected by that.
        block_on(Font::load(&Immediate(&doc), obj.as_dict().unwrap()))
    }

    #[test]
    fn simple_winansi_font() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
            &[],
            &[],
        );
        assert!(f.simple);
        assert_eq!(f.decode(65), "A");
        assert_eq!(f.decode(0x93), "\u{201C}");
        assert_eq!(f.width(one(65)), 500.0);
        assert!(f.is_space(one(32)));
        assert!(!f.is_space(one(65)));
        assert_eq!(f.codes(b"AB"), vec![one(65), one(66)]);
    }

    /// A minimal sfnt whose `cmap` advertises exactly `platforms`. Enough for
    /// [`Font::built_for_windows`], which reads no further than the subtable
    /// records.
    fn sfnt_program(platforms: &[(u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(b"cmap");
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&28u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(platforms.len() as u16).to_be_bytes());
        for &(pid, eid) in platforms {
            out.extend_from_slice(&pid.to_be_bytes());
            out.extend_from_slice(&eid.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        out
    }

    /// A TrueType subset stating no `/Encoding` and no `/ToUnicode`, whose
    /// embedded program says it was built for Windows. Its curly quotes and
    /// dashes sit at codes StandardEncoding does not define, and used to
    /// extract as U+FFFD.
    #[test]
    fn an_embedded_windows_font_reads_its_high_codes_as_winansi() {
        let f = font_from(
            "<< /Type /Font /Subtype /TrueType /BaseFont /OPPEKN+TimesNewRoman \
             /FirstChar 32 /LastChar 215 /FontDescriptor 7 0 R >>",
            &[(6, &sfnt_program(&[(1, 0), (3, 0)]))],
            &[(7, "<< /Type /FontDescriptor /Flags 6 /FontFile2 6 0 R >>")],
        );
        assert_eq!(f.decode(0x92), "\u{2019}", "right single quote");
        assert_eq!(f.decode(0x96), "\u{2013}", "en dash");
        assert_eq!(f.decode(0x97), "\u{2014}", "em dash");
    }

    /// `/Differences` names using the AGL conventions — underscore-joined
    /// ligature components and period-suffixed variants — decode to the text
    /// they represent. Modeled on a real Garamond Premier Pro subset whose
    /// low codes carry `/f_i`, `/T_h`, and `/eight.oldstyle` and used to
    /// extract as U+FFFD.
    #[test]
    fn differences_agl_ligatures_and_variants_decode() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /MCDWFT+GaramondPremrPro \
             /Encoding << /Type /Encoding /BaseEncoding /WinAnsiEncoding \
             /Differences [ 1 /f_i /T_h /eight.oldstyle /x.sc ] >> >>",
            &[],
            &[],
        );
        assert_eq!(f.decode(1), "fi");
        assert_eq!(f.decode(2), "Th");
        assert_eq!(f.decode(3), "8");
        assert_eq!(f.decode(4), "x");
        assert_eq!(f.decode(65), "A"); // the base table is untouched
    }

    /// A `/ToUnicode` entry of `<FFFD>` outranks nothing: the `/Differences`
    /// name for the same code is real evidence and must win. Modeled on a
    /// Brill journal header whose page number `314` extracted as `3\u{FFFD}4`
    /// because the producer wrote `<13> <FFFD>` for `/one.SP`.
    #[test]
    fn a_differences_name_outranks_a_replacement_tounicode_entry() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /GKDCHH+Brill-Roman \
             /ToUnicode 6 0 R \
             /Encoding << /Type /Encoding /Differences [ 19 /one.SP ] >> >>",
            &[(6, b"1 beginbfchar <13> <FFFD> endbfchar")],
            &[],
        );
        assert_eq!(f.decode(0x13), "1");
    }

    /// The guess requires evidence. A font with no program at all offers none,
    /// and measuring the guess without it against a real corpus turned one
    /// document's apostrophes into `Õ` and four documents' dingbats into
    /// thorns — so these codes stay U+FFFD rather than become plausible-looking
    /// nonsense.
    #[test]
    fn a_font_with_no_program_does_not_guess_at_its_high_codes() {
        let f = font_from(
            "<< /Type /Font /Subtype /TrueType /BaseFont /TimesNewRoman \
             /FirstChar 32 /LastChar 215 >>",
            &[],
            &[],
        );
        assert_eq!(f.decode(0x92), "\u{FFFD}");
        assert_eq!(f.decode(0x96), "\u{FFFD}");
    }

    /// A dingbat face is structurally indistinguishable from a text subset by
    /// the same producer, so it is excluded by name. Reading its bullets as
    /// WinAnsi turned a real document's list markers into `Ø`.
    #[test]
    fn a_picture_font_does_not_guess_at_its_high_codes() {
        for base in [
            "/JOGDGG+Wingdings",
            "/Wingdings",
            "/Symbol,Italic",
            "/ZapfDingbats",
            "/Webdings",
        ] {
            let f = font_from(
                &format!(
                    "<< /Type /Font /Subtype /TrueType /BaseFont {base} \
                     /FontDescriptor 7 0 R >>"
                ),
                &[(6, &sfnt_program(&[(1, 0), (3, 0)]))],
                &[(7, "<< /Type /FontDescriptor /Flags 4 /FontFile2 6 0 R >>")],
            );
            assert!(!f.winansi_high_codes, "{base} must not guess");
            assert_eq!(f.decode(0xD8), "\u{FFFD}", "{base}");
        }
    }

    /// The name check keys on the family, so a text face is not excluded just
    /// for containing a picture family's name inside a longer one.
    #[test]
    fn the_picture_font_names_do_not_swallow_text_faces() {
        for base in ["/ABCDEF+SymbolicSans", "/Wingding", "/TimesNewRoman,Bold"] {
            let f = font_from(
                &format!(
                    "<< /Type /Font /Subtype /TrueType /BaseFont {base} \
                     /FontDescriptor 7 0 R >>"
                ),
                &[(6, &sfnt_program(&[(3, 0)]))],
                &[(7, "<< /Type /FontDescriptor /Flags 6 /FontFile2 6 0 R >>")],
            );
            assert!(f.winansi_high_codes, "{base} is a text face");
        }
    }

    /// Nor does a program built for the Macintosh: 0x92 is `í` there, not an
    /// apostrophe, so WinAnsiEncoding would be actively wrong.
    #[test]
    fn a_macintosh_only_font_does_not_guess_at_its_high_codes() {
        let f = font_from(
            "<< /Type /Font /Subtype /TrueType /BaseFont /Palatino \
             /FirstChar 32 /LastChar 215 /FontDescriptor 7 0 R >>",
            &[(6, &sfnt_program(&[(1, 0)]))],
            &[(7, "<< /Type /FontDescriptor /Flags 34 /FontFile2 6 0 R >>")],
        );
        assert_eq!(f.decode(0x92), "\u{FFFD}");
        assert_eq!(f.decode(0xD5), "\u{FFFD}");
    }

    /// The fallback fills gaps and nothing else: every code StandardEncoding
    /// defines must still decode to what it says, even for the font that is
    /// allowed to guess, or a font that was reading correctly would quietly
    /// change meaning.
    #[test]
    fn the_winansi_fallback_never_overrides_standard_encoding() {
        let f = font_from(
            "<< /Type /Font /Subtype /TrueType /BaseFont /OPPEKN+TimesNewRoman \
             /FontDescriptor 7 0 R >>",
            &[(6, &sfnt_program(&[(3, 0)]))],
            &[(7, "<< /Type /FontDescriptor /Flags 6 /FontFile2 6 0 R >>")],
        );
        assert!(f.winansi_high_codes, "this font is allowed to guess");
        for byte in 0..=255u8 {
            let Some(want) = encodings::standard(byte) else {
                continue;
            };
            assert_eq!(
                f.decode(u32::from(byte)),
                want.to_string(),
                "code {byte:#04X} must keep its StandardEncoding character",
            );
        }
    }

    /// A `/FontFile2` whose trailing `/Filter` is an image codec is refused
    /// by the checked fetch, so its bytes — a passthrough codestream, here
    /// deliberately a valid sfnt to prove the refusal happens on the label —
    /// contribute no WinAnsi evidence. Before the refusal, whatever the
    /// codestream happened to contain decided how codes >= 128 read.
    #[test]
    fn an_image_codec_font_program_offers_no_winansi_evidence() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /TrueType /BaseFont /OPPEKN+TimesNewRoman \
             /FontDescriptor 7 0 R >>",
        );
        b.stream(6, "/Filter /DCTDecode", &sfnt_program(&[(3, 0)]));
        b.object(7, "<< /Type /FontDescriptor /Flags 6 /FontFile2 6 0 R >>");
        let doc = Document::load(b.build(1)).unwrap();
        let obj = doc.get(ObjRef { num: 5, gen: 0 }).unwrap();
        let f = block_on(Font::load(&Immediate(&doc), obj.as_dict().unwrap()));
        assert!(
            !f.winansi_high_codes,
            "a refused program is no evidence at all"
        );
        assert_eq!(f.decode(0x92), "\u{FFFD}");
    }

    /// Builds a document whose object 5 is a Type1 font carrying the given
    /// `/ToUnicode` stream. The CMap maps code 65 to U+03A9, so the two
    /// outcomes read apart: `Ω` when the mapping is honored, `A` (falling
    /// through to StandardEncoding) when the stream is refused.
    fn font_with_tounicode(stream_dict: &str, data: &[u8]) -> Font {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        );
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 8 0 R >>",
        );
        b.stream(8, stream_dict, data);
        let doc = Document::load(b.build(1)).unwrap();
        let obj = doc.get(ObjRef { num: 5, gen: 0 }).unwrap();
        block_on(Font::load(&Immediate(&doc), obj.as_dict().unwrap()))
    }

    const OMEGA_CMAP: &[u8] = b"1 begincodespacerange <00> <FF> endcodespacerange\n\
                                1 beginbfchar <41> <03A9> endbfchar";

    /// A `/ToUnicode` whose trailing `/Filter` is an image codec is refused,
    /// never token-scanned: the bytes here are deliberately a valid CMap to
    /// prove the refusal happens on the label, not on the content.
    #[test]
    fn an_image_codec_tounicode_is_refused_not_parsed() {
        let f = font_with_tounicode("/Filter /DCTDecode", OMEGA_CMAP);
        assert_eq!(f.decode(65), "A", "the mapping must not apply");
    }

    /// The inverse of the refusal: a benign trailing filter the decoder can
    /// run (here ASCIIHexDecode) must keep working — over-refusal would
    /// silently strip the mappings from every compressed ToUnicode.
    #[test]
    fn a_hex_encoded_tounicode_still_reads() {
        let hex: Vec<u8> = OMEGA_CMAP
            .iter()
            .flat_map(|b| format!("{b:02X}").into_bytes())
            .chain(*b">")
            .collect();
        let f = font_with_tounicode("/Filter /ASCIIHexDecode", &hex);
        assert_eq!(f.decode(65), "\u{3A9}", "the mapping must apply");
    }

    #[test]
    fn differences_and_widths() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Custom \
             /Encoding << /BaseEncoding /WinAnsiEncoding \
             /Differences [65 /alpha /uni0042] >> \
             /FirstChar 65 /Widths [600 700] >>",
            &[],
            &[],
        );
        assert_eq!(f.decode(65), "\u{3B1}"); // /alpha
        assert_eq!(f.decode(66), "B"); // /uni0042
        assert_eq!(f.decode(67), "C"); // untouched base
        assert_eq!(f.width(one(65)), 600.0);
        assert_eq!(f.width(one(66)), 700.0);
        assert_eq!(f.width(one(67)), 500.0); // default
    }

    #[test]
    fn missing_width_from_descriptor() {
        let f = font_from(
            "<< /Type /Font /Subtype /TrueType /BaseFont /X \
             /FontDescriptor << /Type /FontDescriptor /MissingWidth 300 >> >>",
            &[],
            &[],
        );
        assert_eq!(f.width(one(65)), 300.0);
    }

    #[test]
    fn tounicode_beats_encoding() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /X \
             /Encoding /WinAnsiEncoding /ToUnicode 6 0 R >>",
            &[(6, b"1 beginbfchar <41> <0058> endbfchar")],
            &[],
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
            &[],
        );
        assert!(!f.simple);
        assert_eq!(f.codes(b"\x00\x01\x00\x02"), vec![two(1), two(2)]);
        assert_eq!(
            f.codes(b"\x00\x01\x07"),
            vec![two(1), CharCode { code: 7, len: 1 }]
        ); // odd tail
        assert_eq!(f.decode(1), "\u{3A9}");
        assert_eq!(f.decode(2), "\u{FFFD}"); // ToUnicode only for Type0
        assert_eq!(f.width(two(1)), 500.0);
        assert_eq!(f.width(two(2)), 600.0);
        assert_eq!(f.width(two(10)), 250.0);
        assert_eq!(f.width(two(12)), 250.0);
        assert_eq!(f.width(two(99)), 800.0); // /DW
        assert!(!f.is_space(two(32)));
    }

    /// A predefined Shift-JIS CMap: 1-byte and 2-byte codes split by the
    /// codespaces, `/W` keys on the CID the CMap yields (843 for あ, not
    /// the code 0x82A0), and with no `/ToUnicode` the Japan1 collection's
    /// own mapping supplies the Unicode.
    #[test]
    fn type0_predefined_rksj_maps_codes_to_cids() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /90ms-RKSJ-H \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType0 \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 2 >> \
             /DW 1000 /W [843 [750]] >>] >>",
            &[],
            &[],
        );
        assert!(!f.simple);
        assert!(!f.vertical);
        assert!(f.encoding_known);
        assert_eq!(f.codes(b"A\x82\xA0"), vec![one(65), two(0x82A0)]);
        assert_eq!(f.width(two(0x82A0)), 750.0);
        assert_eq!(f.width(two(0x82A1)), 1000.0); // /DW
        let mut s = String::new();
        f.decode_into(two(0x82A0), &mut s);
        assert_eq!(s, "あ");
        assert!(f.is_space(one(32)), "RKSJ reads byte 32 as a 1-byte code");
        assert!(!f.is_space(two(32)));
    }

    /// The vertical form: WMode from the CMap, `/W2` displacements keyed
    /// on the CID (the rotated variant's), `/DW2` for the rest, and
    /// Unicode answered by the horizontal base so punctuation reads as
    /// the character rather than a presentation form.
    #[test]
    fn type0_predefined_rksj_vertical() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /90ms-RKSJ-V \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType0 \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) >> \
             /DW2 [880 -1200] /W2 [7887 [-500 250 880]] >>] >>",
            &[],
            &[],
        );
        assert!(f.vertical);
        assert_eq!(f.vwidth(two(0x8141)), -500.0); // 、 maps to CID 7887
        assert_eq!(f.vwidth(two(0x82A0)), -1200.0); // /DW2
        let mut s = String::new();
        f.decode_into(two(0x8141), &mut s);
        assert_eq!(s, "、");
    }

    /// The `c1 c2 w1 vx vy` form of `/W2`, and the -1000 default without
    /// a `/DW2`.
    #[test]
    fn w2_range_form_and_default() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-V \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 \
             /W2 [10 12 -800 500 880] >>] >>",
            &[],
            &[],
        );
        assert!(f.vertical, "Identity-V is writing mode 1");
        assert_eq!(f.vwidth(two(10)), -800.0);
        assert_eq!(f.vwidth(two(12)), -800.0);
        assert_eq!(f.vwidth(two(13)), -1000.0); // default
    }

    /// An embedded CMap stream as `/Encoding`: 1-byte codes split per its
    /// codespace and map through its cidranges — previously ignored and
    /// read as 2-byte identity.
    #[test]
    fn type0_embedded_cmap_stream() {
        let cmap: &[u8] = b"1 begincodespacerange <00> <FF> endcodespacerange\n\
                            1 begincidrange <41> <5A> 100 endcidrange";
        let f = font_from(
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding 6 0 R \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 \
             /DW 1000 /W [100 [650]] >>] >>",
            &[(6, cmap)],
            &[],
        );
        assert!(f.encoding_known);
        assert_eq!(f.codes(b"AB"), vec![one(0x41), one(0x42)]);
        assert_eq!(f.width(one(0x41)), 650.0); // /W keys on CID 100
        assert_eq!(f.width(one(0x42)), 1000.0);
    }

    /// A named CMap that resolves to nothing keeps today's behavior —
    /// 2-byte code==CID — and says so on the loaded font.
    #[test]
    fn type0_unresolvable_encoding_stays_identity_and_is_noted() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /NotACMap-H \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 \
             /DW 800 >>] >>",
            &[],
            &[],
        );
        assert!(!f.encoding_known);
        assert!(!f.vertical);
        assert_eq!(f.codes(b"\x00\x41"), vec![two(0x41)]);
        assert_eq!(f.width(two(0x41)), 800.0);
    }

    #[test]
    fn huge_first_char_widths_do_not_overflow() {
        // /FirstChar u32::MAX: the second /Widths entry would overflow.
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /FirstChar 4294967295 /Widths [600 700] >>",
            &[],
            &[],
        );
        assert_eq!(f.width(one(u32::MAX)), 600.0);
        assert_eq!(f.width(one(65)), 500.0); // overflowed entry dropped
    }

    #[test]
    fn differences_start_at_u32_max_does_not_overflow() {
        let f = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding << /Differences [4294967295 /a /b] >> >>",
            &[],
            &[],
        );
        assert_eq!(f.decode(65), "A"); // base table untouched, no panic
                                       // Same start code reached via a Real that saturates to u32::MAX.
        let g = font_from(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding << /Differences [5000000000.0 /a /b] >> >>",
            &[],
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
            &[],
        );
        assert_eq!(f.width(two(u32::MAX)), 10.0);
        assert_eq!(f.width(two(0)), 800.0); // overflowed entry dropped -> /DW
    }

    #[test]
    fn fallback_font_uses_standard() {
        let f = Font::fallback();
        assert_eq!(f.decode(65), "A");
        assert_eq!(f.decode(0xA9), "\u{27}"); // Standard quotesingle
        assert_eq!(f.decode(0), "\u{FFFD}");
        assert_eq!(f.width(one(65)), 500.0);
    }
}
