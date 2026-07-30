//! Font loading from a page's `/Font` resource dictionary: simple fonts
//! (byte codes, `/Encoding` + `/Differences`, `/Widths`) and Type0/CID
//! fonts (2-byte codes, `/ToUnicode`, descendant `/W` + `/DW`).

use crate::cmap::ToUnicode;
use crate::sfnt;
use pdfboss_core::{AsyncObjectSource, Dict, Object};
use pdfboss_encoding as encodings;
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
    /// The font states no `/Encoding`, but its embedded program advertises a
    /// Microsoft `cmap` — so codes StandardEncoding leaves undefined are read
    /// as WinAnsiEncoding. See [`Font::decode_into`] for why that evidence is
    /// required rather than assumed.
    winansi_high_codes: bool,
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
            encoding: None,
            widths: HashMap::new(),
            default_width: 500.0,
            space_code: Some(32),
            winansi_high_codes: false,
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
    /// the lower-priority mappings still get their chance.
    async fn load_to_unicode<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Option<ToUnicode> {
        let obj = rv(src, dict, "ToUnicode").await?;
        let data = src.stream_data(obj.as_stream()?).await.ok()?;
        let cmap = ToUnicode::parse(&data);
        (!cmap.is_empty()).then_some(cmap)
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

    /// Loads a Type1/TrueType/Type3 font: 1-byte codes, `/Encoding` base
    /// plus `/Differences`, widths from `/FirstChar` + `/Widths`.
    async fn load_simple<S: AsyncObjectSource>(
        src: &S,
        dict: &Dict,
        to_unicode: Option<ToUnicode>,
    ) -> Font {
        let encoding = Font::load_encoding(src, dict).await;

        let mut widths = HashMap::new();
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

        Font {
            simple: true,
            to_unicode,
            encoding,
            widths,
            default_width,
            space_code: Some(32),
            winansi_high_codes,
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
    async fn sfnt_program<S: AsyncObjectSource>(
        src: &S,
        descriptor: &Dict,
        key: &str,
    ) -> Option<Vec<u8>> {
        let obj = rv(src, descriptor, key).await?;
        src.stream_data(obj.as_stream()?).await.ok()
    }

    /// Builds the 256-entry Unicode table from `/Encoding`: a base table
    /// (named directly or via `/BaseEncoding`, default Standard) with
    /// `/Differences` glyph names applied on top.
    async fn load_encoding<S: AsyncObjectSource>(
        src: &S,
        dict: &Dict,
    ) -> Option<Box<[Option<char>; 256]>> {
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
        let mut table = Box::new([None; 256]);
        for (code, slot) in table.iter_mut().enumerate() {
            *slot = base(code as u8);
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
    async fn load_type0<S: AsyncObjectSource>(
        src: &S,
        dict: &Dict,
        to_unicode: Option<ToUnicode>,
    ) -> Font {
        let descendant = Font::load_descendant(src, dict).await;

        let mut widths = HashMap::new();
        let mut default_width = 1000.0;
        if let Some(desc) = &descendant {
            if let Some(dw) = rv(src, desc, "DW").await.and_then(|o| o.as_f64()) {
                default_width = dw as f32;
            }
            if let Some(Object::Array(w)) = rv(src, desc, "W").await {
                Font::parse_cid_widths(src, &w, &mut widths).await;
            }
        }

        Font {
            simple: false,
            to_unicode,
            encoding: None,
            widths,
            default_width,
            space_code: None,
            // Two-byte codes never reach the single-byte fallbacks.
            winansi_high_codes: false,
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
        widths: &mut HashMap<u32, f32>,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{block_on, Document, Immediate, ObjRef};
    use pdfboss_testkit::PdfBuilder;

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
        assert_eq!(f.width(65), 500.0);
        assert!(f.is_space(32));
        assert!(!f.is_space(65));
        assert_eq!(f.codes(b"AB"), vec![65, 66]);
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
