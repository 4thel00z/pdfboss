//! Shared PDF font-encoding tables (WinAnsi / MacRoman / Standard, from
//! ISO 32000 Appendix D), glyph-name-to-Unicode resolution over the Adobe
//! Glyph List, and the built-in encoding of a Type 1 font program, consumed
//! by the pdfboss text-extraction and rendering crates.

mod afm;
mod agl;
pub use afm::{is_standard_14, standard_14_width};

/// WinAnsiEncoding codes `0x80..=0x9F` (the region that differs from
/// Latin-1); `None` marks unassigned codes.
const WIN_ANSI_80_9F: [Option<char>; 32] = [
    Some('\u{20AC}'),
    None,
    Some('\u{201A}'),
    Some('\u{0192}'),
    Some('\u{201E}'),
    Some('\u{2026}'),
    Some('\u{2020}'),
    Some('\u{2021}'),
    Some('\u{02C6}'),
    Some('\u{2030}'),
    Some('\u{0160}'),
    Some('\u{2039}'),
    Some('\u{0152}'),
    None,
    Some('\u{017D}'),
    None,
    None,
    Some('\u{2018}'),
    Some('\u{2019}'),
    Some('\u{201C}'),
    Some('\u{201D}'),
    Some('\u{2022}'),
    Some('\u{2013}'),
    Some('\u{2014}'),
    Some('\u{02DC}'),
    Some('\u{2122}'),
    Some('\u{0161}'),
    Some('\u{203A}'),
    Some('\u{0153}'),
    None,
    Some('\u{017E}'),
    Some('\u{0178}'),
];

/// Unicode value of `code` in `WinAnsiEncoding`.
pub fn win_ansi(code: u8) -> Option<char> {
    match code {
        0x20..=0x7E => Some(code as char),
        0x80..=0x9F => WIN_ANSI_80_9F[(code - 0x80) as usize],
        0xA0..=0xFF => Some(code as char),
        _ => None,
    }
}

/// WinAnsiEncoding glyph name for `code` (ISO 32000-1 Annex D.2
/// "WinAnsiEncoding" column). `None` for exactly the codes [`win_ansi`]
/// leaves unassigned. Two ASCII codes diverge from StandardEncoding's
/// names: `0x27` is `quotesingle` and `0x60` is `grave` (the straight
/// marks, matching `win_ansi`'s identity mapping there). Two codes render
/// an existing glyph rather than owning one: `0xA0` carries `space` (the
/// nonbreaking space draws as the space glyph) and `0xAD` carries `hyphen`
/// (likewise the soft hyphen) — see the self-verifying
/// `win_ansi_glyph_name_matches_win_ansi_table` test below.
pub fn win_ansi_glyph_name(code: u8) -> Option<&'static str> {
    match code {
        0x27 => Some("quotesingle"),
        0x60 => Some("grave"),
        0x20..=0x7E => Some(STANDARD_ASCII_NAMES[(code - 0x20) as usize]),
        0x80..=0x9F => WIN_ANSI_80_9F_NAMES[(code - 0x80) as usize],
        0xA0..=0xFF => Some(WIN_ANSI_A0_FF_NAMES[(code - 0xA0) as usize]),
        _ => None,
    }
}

/// WinAnsiEncoding glyph names for codes `0x80..=0x9F`, parallel to
/// [`WIN_ANSI_80_9F`]; `None` marks the same unassigned codes.
const WIN_ANSI_80_9F_NAMES: [Option<&str>; 32] = [
    Some("Euro"),
    None,
    Some("quotesinglbase"),
    Some("florin"),
    Some("quotedblbase"),
    Some("ellipsis"),
    Some("dagger"),
    Some("daggerdbl"),
    Some("circumflex"),
    Some("perthousand"),
    Some("Scaron"),
    Some("guilsinglleft"),
    Some("OE"),
    None,
    Some("Zcaron"),
    None,
    None,
    Some("quoteleft"),
    Some("quoteright"),
    Some("quotedblleft"),
    Some("quotedblright"),
    Some("bullet"),
    Some("endash"),
    Some("emdash"),
    Some("tilde"),
    Some("trademark"),
    Some("scaron"),
    Some("guilsinglright"),
    Some("oe"),
    None,
    Some("zcaron"),
    Some("Ydieresis"),
];

/// WinAnsiEncoding glyph names for codes `0xA0..=0xFF` (ISO 32000-1
/// Annex D.2 "WinAnsiEncoding" column), in code order (index `0` is code
/// `0xA0`).
const WIN_ANSI_A0_FF_NAMES: [&str; 96] = [
    "space",
    "exclamdown",
    "cent",
    "sterling",
    "currency",
    "yen",
    "brokenbar",
    "section",
    "dieresis",
    "copyright",
    "ordfeminine",
    "guillemotleft",
    "logicalnot",
    "hyphen",
    "registered",
    "macron",
    "degree",
    "plusminus",
    "twosuperior",
    "threesuperior",
    "acute",
    "mu",
    "paragraph",
    "periodcentered",
    "cedilla",
    "onesuperior",
    "ordmasculine",
    "guillemotright",
    "onequarter",
    "onehalf",
    "threequarters",
    "questiondown",
    "Agrave",
    "Aacute",
    "Acircumflex",
    "Atilde",
    "Adieresis",
    "Aring",
    "AE",
    "Ccedilla",
    "Egrave",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Igrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Eth",
    "Ntilde",
    "Ograve",
    "Oacute",
    "Ocircumflex",
    "Otilde",
    "Odieresis",
    "multiply",
    "Oslash",
    "Ugrave",
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Yacute",
    "Thorn",
    "germandbls",
    "agrave",
    "aacute",
    "acircumflex",
    "atilde",
    "adieresis",
    "aring",
    "ae",
    "ccedilla",
    "egrave",
    "eacute",
    "ecircumflex",
    "edieresis",
    "igrave",
    "iacute",
    "icircumflex",
    "idieresis",
    "eth",
    "ntilde",
    "ograve",
    "oacute",
    "ocircumflex",
    "otilde",
    "odieresis",
    "divide",
    "oslash",
    "ugrave",
    "uacute",
    "ucircumflex",
    "udieresis",
    "yacute",
    "thorn",
    "ydieresis",
];

/// MacRomanEncoding codes `0x80..=0xFF` (codes below coincide with ASCII).
const MAC_ROMAN_HIGH: [char; 128] = [
    '\u{C4}', '\u{C5}', '\u{C7}', '\u{C9}', '\u{D1}', '\u{D6}', '\u{DC}', '\u{E1}', '\u{E0}',
    '\u{E2}', '\u{E4}', '\u{E3}', '\u{E5}', '\u{E7}', '\u{E9}', '\u{E8}', '\u{EA}', '\u{EB}',
    '\u{ED}', '\u{EC}', '\u{EE}', '\u{EF}', '\u{F1}', '\u{F3}', '\u{F2}', '\u{F4}', '\u{F6}',
    '\u{F5}', '\u{FA}', '\u{F9}', '\u{FB}', '\u{FC}', '\u{2020}', '\u{B0}', '\u{A2}', '\u{A3}',
    '\u{A7}', '\u{2022}', '\u{B6}', '\u{DF}', '\u{AE}', '\u{A9}', '\u{2122}', '\u{B4}', '\u{A8}',
    '\u{2260}', '\u{C6}', '\u{D8}', '\u{221E}', '\u{B1}', '\u{2264}', '\u{2265}', '\u{A5}',
    '\u{B5}', '\u{2202}', '\u{2211}', '\u{220F}', '\u{3C0}', '\u{222B}', '\u{AA}', '\u{BA}',
    '\u{3A9}', '\u{E6}', '\u{F8}', '\u{BF}', '\u{A1}', '\u{AC}', '\u{221A}', '\u{192}', '\u{2248}',
    '\u{2206}', '\u{AB}', '\u{BB}', '\u{2026}', '\u{A0}', '\u{C0}', '\u{C3}', '\u{D5}', '\u{152}',
    '\u{153}', '\u{2013}', '\u{2014}', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '\u{F7}',
    '\u{25CA}', '\u{FF}', '\u{178}', '\u{2044}', '\u{20AC}', '\u{2039}', '\u{203A}', '\u{FB01}',
    '\u{FB02}', '\u{2021}', '\u{B7}', '\u{201A}', '\u{201E}', '\u{2030}', '\u{C2}', '\u{CA}',
    '\u{C1}', '\u{CB}', '\u{C8}', '\u{CD}', '\u{CE}', '\u{CF}', '\u{CC}', '\u{D3}', '\u{D4}',
    '\u{F8FF}', '\u{D2}', '\u{DA}', '\u{DB}', '\u{D9}', '\u{131}', '\u{2C6}', '\u{2DC}', '\u{AF}',
    '\u{2D8}', '\u{2D9}', '\u{2DA}', '\u{B8}', '\u{2DD}', '\u{2DB}', '\u{2C7}',
];

/// Unicode value of `code` in `MacRomanEncoding`.
pub fn mac_roman(code: u8) -> Option<char> {
    match code {
        0x20..=0x7E => Some(code as char),
        0x80..=0xFF => Some(MAC_ROMAN_HIGH[(code - 0x80) as usize]),
        _ => None,
    }
}

/// StandardEncoding codes above 0x7E that are assigned (sparse).
const STANDARD_HIGH: &[(u8, char)] = &[
    (0xA1, '\u{A1}'),
    (0xA2, '\u{A2}'),
    (0xA3, '\u{A3}'),
    (0xA4, '\u{2044}'),
    (0xA5, '\u{A5}'),
    (0xA6, '\u{192}'),
    (0xA7, '\u{A7}'),
    (0xA8, '\u{A4}'),
    (0xA9, '\u{27}'),
    (0xAA, '\u{201C}'),
    (0xAB, '\u{AB}'),
    (0xAC, '\u{2039}'),
    (0xAD, '\u{203A}'),
    (0xAE, '\u{FB01}'),
    (0xAF, '\u{FB02}'),
    (0xB1, '\u{2013}'),
    (0xB2, '\u{2020}'),
    (0xB3, '\u{2021}'),
    (0xB4, '\u{B7}'),
    (0xB6, '\u{B6}'),
    (0xB7, '\u{2022}'),
    (0xB8, '\u{201A}'),
    (0xB9, '\u{201E}'),
    (0xBA, '\u{201D}'),
    (0xBB, '\u{BB}'),
    (0xBC, '\u{2026}'),
    (0xBD, '\u{2030}'),
    (0xBF, '\u{BF}'),
    (0xC1, '\u{60}'),
    (0xC2, '\u{B4}'),
    (0xC3, '\u{2C6}'),
    (0xC4, '\u{2DC}'),
    (0xC5, '\u{AF}'),
    (0xC6, '\u{2D8}'),
    (0xC7, '\u{2D9}'),
    (0xC8, '\u{A8}'),
    (0xCA, '\u{2DA}'),
    (0xCB, '\u{B8}'),
    (0xCD, '\u{2DD}'),
    (0xCE, '\u{2DB}'),
    (0xCF, '\u{2C7}'),
    (0xD0, '\u{2014}'),
    (0xE1, '\u{C6}'),
    (0xE3, '\u{AA}'),
    (0xE8, '\u{141}'),
    (0xE9, '\u{D8}'),
    (0xEA, '\u{152}'),
    (0xEB, '\u{BA}'),
    (0xF1, '\u{E6}'),
    (0xF5, '\u{131}'),
    (0xF8, '\u{142}'),
    (0xF9, '\u{F8}'),
    (0xFA, '\u{153}'),
    (0xFB, '\u{DF}'),
];

/// Unicode value of `code` in `StandardEncoding`.
pub fn standard(code: u8) -> Option<char> {
    match code {
        0x27 => Some('\u{2019}'),
        0x60 => Some('\u{2018}'),
        0x20..=0x7E => Some(code as char),
        0xA1..=0xFF => STANDARD_HIGH
            .iter()
            .find(|&&(c, _)| c == code)
            .map(|&(_, u)| u),
        _ => None,
    }
}

/// StandardEncoding names for codes `0x20..=0x7E` (space..asciitilde), in
/// code order (index `0` is code `0x20`). Two codes diverge from their plain
/// ASCII name: `0x27` is `quoteright` (a curly right quote, not the straight
/// `quotesingle` apostrophe) and `0x60` is `quoteleft` (a curly left quote,
/// not `grave`) -- matching `standard`'s `0x27`/`0x60` special cases above.
const STANDARD_ASCII_NAMES: [&str; 95] = [
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quoteright",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "quoteleft",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
];

/// StandardEncoding names for codes above `0x7E` (ISO 32000-1 Annex D.2
/// "StandardEncoding" column), parallel to [`STANDARD_HIGH`]'s codes, in the
/// same order.
const STANDARD_HIGH_NAMES: &[(u8, &str)] = &[
    (0xA1, "exclamdown"),
    (0xA2, "cent"),
    (0xA3, "sterling"),
    (0xA4, "fraction"),
    (0xA5, "yen"),
    (0xA6, "florin"),
    (0xA7, "section"),
    (0xA8, "currency"),
    (0xA9, "quotesingle"),
    (0xAA, "quotedblleft"),
    (0xAB, "guillemotleft"),
    (0xAC, "guilsinglleft"),
    (0xAD, "guilsinglright"),
    (0xAE, "fi"),
    (0xAF, "fl"),
    (0xB1, "endash"),
    (0xB2, "dagger"),
    (0xB3, "daggerdbl"),
    (0xB4, "periodcentered"),
    (0xB6, "paragraph"),
    (0xB7, "bullet"),
    (0xB8, "quotesinglbase"),
    (0xB9, "quotedblbase"),
    (0xBA, "quotedblright"),
    (0xBB, "guillemotright"),
    (0xBC, "ellipsis"),
    (0xBD, "perthousand"),
    (0xBF, "questiondown"),
    (0xC1, "grave"),
    (0xC2, "acute"),
    (0xC3, "circumflex"),
    (0xC4, "tilde"),
    (0xC5, "macron"),
    (0xC6, "breve"),
    (0xC7, "dotaccent"),
    (0xC8, "dieresis"),
    (0xCA, "ring"),
    (0xCB, "cedilla"),
    (0xCD, "hungarumlaut"),
    (0xCE, "ogonek"),
    (0xCF, "caron"),
    (0xD0, "emdash"),
    (0xE1, "AE"),
    (0xE3, "ordfeminine"),
    (0xE8, "Lslash"),
    (0xE9, "Oslash"),
    (0xEA, "OE"),
    (0xEB, "ordmasculine"),
    (0xF1, "ae"),
    (0xF5, "dotlessi"),
    (0xF8, "lslash"),
    (0xF9, "oslash"),
    (0xFA, "oe"),
    (0xFB, "germandbls"),
];

/// Adobe StandardEncoding glyph name for `code` (ISO 32000-1 Annex D.2
/// "StandardEncoding" column; equivalently Adobe Type 1 Font Format
/// Appendix C). `None` for exactly the codes `standard` leaves unassigned
/// (see the self-verifying `standard_encoding_name_matches_standard_table`
/// test below, which ties this table to that one so an authoring mistake
/// here fails a test rather than silently mis-encoding a glyph).
pub fn standard_encoding_name(code: u8) -> Option<&'static str> {
    match code {
        0x20..=0x7E => Some(STANDARD_ASCII_NAMES[(code - 0x20) as usize]),
        0xA1..=0xFF => STANDARD_HIGH_NAMES
            .iter()
            .find(|&&(c, _)| c == code)
            .map(|&(_, n)| n),
        _ => None,
    }
}

/// Resolves a glyph name (as used in `/Differences`) to a Unicode scalar:
/// `uniXXXX` and `uXXXX`–`uXXXXXX` hex forms, single ASCII letters, the
/// Adobe Glyph List, and the TeX math names the list never adopted. `None`
/// for an unknown name, and for a listed name whose text is more than one
/// scalar; [`glyph_to_text`] resolves those.
pub fn glyph_to_unicode(name: &str) -> Option<char> {
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return char::from_u32(u32::from_str_radix(hex, 16).ok()?);
        }
    }
    if let Some(hex) = name.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return char::from_u32(u32::from_str_radix(hex, 16).ok()?);
        }
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some(c);
        }
    }
    if let Some(text) = agl_text(name) {
        let mut chars = text.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        };
    }
    tex_glyph(name)
}

/// Resolves a glyph name to the text it represents, per the Adobe Glyph
/// List algorithm: everything from the first period on is dropped
/// (`eight.oldstyle` → `8`), underscore-joined components each resolve and
/// concatenate (`f_i` → `fi`, `T_h` → `Th`), and a `uni` prefix may carry
/// several 4-digit hex groups. `None` unless every component resolves —
/// a partially-resolved ligature would silently drop letters, where the
/// caller's U+FFFD at least stays visible.
pub fn glyph_to_text(name: &str) -> Option<String> {
    let base = name.split('.').next().unwrap_or_default();
    if base.is_empty() {
        return None;
    }
    let mut out = String::new();
    for component in base.split('_') {
        push_component(component, &mut out)?;
    }
    Some(out)
}

/// Appends one underscore-separated component of a glyph name; `None` when
/// the component resolves to nothing.
fn push_component(component: &str, out: &mut String) -> Option<()> {
    let hex = component.strip_prefix("uni").unwrap_or_default();
    if hex.len() >= 8 && hex.len().is_multiple_of(4) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        // Multi-group form: `uni20AC0308` is two scalars. The single-group
        // form stays on the `glyph_to_unicode` path below.
        for group in hex.as_bytes().chunks(4) {
            let group = std::str::from_utf8(group).ok()?;
            let scalar = u32::from_str_radix(group, 16).ok()?;
            out.push(char::from_u32(scalar)?);
        }
        return Some(());
    }
    if let Some(text) = agl_text(component) {
        out.push_str(text);
        return Some(());
    }
    out.push(glyph_to_unicode(component)?);
    Some(())
}

/// The Adobe Glyph List text for `name`, when the list carries it.
fn agl_text(name: &str) -> Option<&'static str> {
    let index = agl::AGL.binary_search_by(|(n, _)| n.cmp(&name)).ok()?;
    Some(agl::AGL[index].1)
}

/// The TeX math name's scalar, when [`GLYPHS_TEX`] carries it.
fn tex_glyph(name: &str) -> Option<char> {
    let index = GLYPHS_TEX.binary_search_by(|(n, _)| n.cmp(&name)).ok()?;
    Some(GLYPHS_TEX[index].1)
}

/// Glyph names of the TeX symbol, math-italic and extension fonts that the
/// Adobe Glyph List does not carry, sorted by name. The extension font's
/// size variants of a delimiter or operator (`parenleftbig`, `parenleftBig`,
/// `summationdisplay`) all stand for the one character.
const GLYPHS_TEX: &[(&str, char)] = &[
    ("Ifractur", '\u{2111}'),
    ("Rfractur", '\u{211C}'),
    ("angbracketleft", '\u{27E8}'),
    ("angbracketleftBig", '\u{27E8}'),
    ("angbracketleftBigg", '\u{27E8}'),
    ("angbracketleftbig", '\u{27E8}'),
    ("angbracketleftbigg", '\u{27E8}'),
    ("angbracketright", '\u{27E9}'),
    ("angbracketrightBig", '\u{27E9}'),
    ("angbracketrightBigg", '\u{27E9}'),
    ("angbracketrightbig", '\u{27E9}'),
    ("angbracketrightbigg", '\u{27E9}'),
    ("arrowbothv", '\u{2195}'),
    ("arrowdblbothv", '\u{21D5}'),
    ("arrowhookleft", '\u{21A9}'),
    ("arrowhookright", '\u{21AA}'),
    ("arrowleftbothalf", '\u{21BD}'),
    ("arrowlefttophalf", '\u{21BC}'),
    ("arrownortheast", '\u{2197}'),
    ("arrownorthwest", '\u{2196}'),
    ("arrowrightbothalf", '\u{21C1}'),
    ("arrowrighttophalf", '\u{21C0}'),
    ("arrowsoutheast", '\u{2198}'),
    ("arrowsouthwest", '\u{2199}'),
    ("asteriskmath", '\u{2217}'),
    ("backslashBig", '\\'),
    ("backslashBigg", '\\'),
    ("backslashbig", '\\'),
    ("backslashbigg", '\\'),
    ("bardbl", '\u{2016}'),
    ("braceleftBig", '{'),
    ("braceleftBigg", '{'),
    ("braceleftbig", '{'),
    ("braceleftbigg", '{'),
    ("bracerightBig", '}'),
    ("bracerightBigg", '}'),
    ("bracerightbig", '}'),
    ("bracerightbigg", '}'),
    ("bracketleftBig", '['),
    ("bracketleftBigg", '['),
    ("bracketleftbig", '['),
    ("bracketleftbigg", '['),
    ("bracketrightBig", ']'),
    ("bracketrightBigg", ']'),
    ("bracketrightbig", ']'),
    ("bracketrightbigg", ']'),
    ("ceilingleft", '\u{2308}'),
    ("ceilingleftBig", '\u{2308}'),
    ("ceilingleftBigg", '\u{2308}'),
    ("ceilingleftbig", '\u{2308}'),
    ("ceilingleftbigg", '\u{2308}'),
    ("ceilingright", '\u{2309}'),
    ("ceilingrightBig", '\u{2309}'),
    ("ceilingrightBigg", '\u{2309}'),
    ("ceilingrightbig", '\u{2309}'),
    ("ceilingrightbigg", '\u{2309}'),
    ("circlecopyrt", '\u{A9}'),
    ("circledivide", '\u{2298}'),
    ("circledot", '\u{2299}'),
    ("circledotdisplay", '\u{2A00}'),
    ("circledottext", '\u{2A00}'),
    ("circleminus", '\u{2296}'),
    ("circlemultiply", '\u{2297}'),
    ("circlemultiplydisplay", '\u{2A02}'),
    ("circlemultiplytext", '\u{2A02}'),
    ("circleplus", '\u{2295}'),
    ("circleplusdisplay", '\u{2A01}'),
    ("circleplustext", '\u{2A01}'),
    ("contintegraldisplay", '\u{222E}'),
    ("contintegraltext", '\u{222E}'),
    ("coproduct", '\u{2210}'),
    ("coproductdisplay", '\u{2210}'),
    ("coproducttext", '\u{2210}'),
    ("diamondmath", '\u{22C4}'),
    ("dotlessj", '\u{237}'),
    ("epsilon1", '\u{3F5}'),
    ("equivasymptotic", '\u{224D}'),
    ("flat", '\u{266D}'),
    ("floorleft", '\u{230A}'),
    ("floorleftBig", '\u{230A}'),
    ("floorleftBigg", '\u{230A}'),
    ("floorleftbig", '\u{230A}'),
    ("floorleftbigg", '\u{230A}'),
    ("floorright", '\u{230B}'),
    ("floorrightBig", '\u{230B}'),
    ("floorrightBigg", '\u{230B}'),
    ("floorrightbig", '\u{230B}'),
    ("floorrightbigg", '\u{230B}'),
    ("followsequal", '\u{227D}'),
    ("greatermuch", '\u{226B}'),
    ("hatwide", '^'),
    ("hatwider", '^'),
    ("hatwidest", '^'),
    ("integraldisplay", '\u{222B}'),
    ("integraltext", '\u{222B}'),
    ("intersectiondisplay", '\u{22C2}'),
    ("intersectionsq", '\u{2293}'),
    ("intersectionsqdisplay", '\u{2A05}'),
    ("intersectionsqtext", '\u{2A05}'),
    ("intersectiontext", '\u{22C2}'),
    ("latticetop", '\u{22A4}'),
    ("lessmuch", '\u{226A}'),
    ("logicalanddisplay", '\u{22C0}'),
    ("logicalandtext", '\u{22C0}'),
    ("logicalordisplay", '\u{22C1}'),
    ("logicalortext", '\u{22C1}'),
    ("lscript", '\u{2113}'),
    ("mapsto", '\u{21A6}'),
    ("minusplus", '\u{2213}'),
    ("natural", '\u{266E}'),
    ("openbullet", '\u{25E6}'),
    ("owner", '\u{220B}'),
    ("parenleftBig", '('),
    ("parenleftBigg", '('),
    ("parenleftbig", '('),
    ("parenleftbigg", '('),
    ("parenrightBig", ')'),
    ("parenrightBigg", ')'),
    ("parenrightbig", ')'),
    ("parenrightbigg", ')'),
    ("phi1", '\u{3D5}'),
    ("pi1", '\u{3D6}'),
    ("precedesequal", '\u{227C}'),
    ("productdisplay", '\u{220F}'),
    ("producttext", '\u{220F}'),
    ("radicalBig", '\u{221A}'),
    ("radicalBigg", '\u{221A}'),
    ("radicalbig", '\u{221A}'),
    ("radicalbigg", '\u{221A}'),
    ("rho1", '\u{3F1}'),
    ("sharp", '\u{266F}'),
    ("sigma1", '\u{3C2}'),
    ("similarequal", '\u{2243}'),
    ("slashBig", '/'),
    ("slashBigg", '/'),
    ("slashbig", '/'),
    ("slashbigg", '/'),
    ("slurabove", '\u{2322}'),
    ("slurbelow", '\u{2323}'),
    ("star", '\u{22C6}'),
    ("subsetsqequal", '\u{2291}'),
    ("summationdisplay", '\u{2211}'),
    ("summationtext", '\u{2211}'),
    ("supersetsqequal", '\u{2292}'),
    ("theta1", '\u{3D1}'),
    ("tie", '\u{2040}'),
    ("tildewide", '~'),
    ("tildewider", '~'),
    ("tildewidest", '~'),
    ("triangle", '\u{25B3}'),
    ("triangleinv", '\u{25BD}'),
    ("triangleleft", '\u{25C1}'),
    ("triangleright", '\u{25B7}'),
    ("turnstileleft", '\u{22A2}'),
    ("turnstileright", '\u{22A3}'),
    ("uniondisplay", '\u{22C3}'),
    ("unionmulti", '\u{228E}'),
    ("unionmultidisplay", '\u{2A04}'),
    ("unionmultitext", '\u{2A04}'),
    ("unionsq", '\u{2294}'),
    ("unionsqdisplay", '\u{2A06}'),
    ("unionsqtext", '\u{2A06}'),
    ("uniontext", '\u{22C3}'),
    ("vector", '\u{20D7}'),
    ("weierstrass", '\u{2118}'),
    ("wreathproduct", '\u{2240}'),
];

/// The built-in encoding of an embedded Type 1 font program: code to glyph
/// name, read from the program's clear-text portion, which is where ISO
/// 32000-1 9.6.6 sends a simple font that states no usable `/Encoding` of
/// its own. A bare `StandardEncoding` token expands to that table, and any
/// `dup <code> /<name> put` entries override it code by code. `None` when
/// the program states no `/Encoding` at all.
pub fn type1_builtin_encoding(program: &[u8]) -> Option<Box<[Option<String>; 256]>> {
    let clear = type1_clear_text(program);
    let mut tokens = PsTokens {
        bytes: clear,
        at: 0,
    };
    tokens.find(|t| *t == b"/Encoding")?;
    let mut table: Box<[Option<String>; 256]> = Box::new(std::array::from_fn(|_| None));
    let tokens: Vec<&[u8]> = tokens.collect();
    if tokens.first() == Some(&b"StandardEncoding".as_slice()) {
        for (code, slot) in table.iter_mut().enumerate() {
            *slot = standard_encoding_name(code as u8).map(String::from);
        }
    }
    for entry in tokens.windows(4) {
        if entry[0] != b"dup" || entry[3] != b"put" {
            continue;
        }
        let Some(code) = std::str::from_utf8(entry[1])
            .ok()
            .and_then(|t| t.parse::<usize>().ok())
        else {
            continue;
        };
        let Some(name) = entry[2]
            .strip_prefix(b"/")
            .and_then(|n| std::str::from_utf8(n).ok())
        else {
            continue;
        };
        if let Some(slot) = table.get_mut(code) {
            *slot = Some(name.to_string());
        }
    }
    Some(table)
}

/// The clear-text portion of a Type 1 program: the type-1 segments of a PFB
/// wrapper, or everything up to the `eexec` token of a raw program (the
/// whole program when there is none).
fn type1_clear_text(program: &[u8]) -> &[u8] {
    if program.first() == Some(&0x80) {
        return pfb_clear_text(program);
    }
    const TOKEN: &[u8] = b"eexec";
    let end = program
        .windows(TOKEN.len())
        .position(|w| w == TOKEN)
        .unwrap_or(program.len());
    &program[..end]
}

/// The first PFB segment's payload when it is clear text; the clear text of
/// every real font is one segment, and a header claiming more bytes than
/// the program holds yields whatever the program does hold.
fn pfb_clear_text(program: &[u8]) -> &[u8] {
    if program.get(1) != Some(&0x01) || program.len() < 6 {
        return &[];
    }
    let len = u32::from_le_bytes([program[2], program[3], program[4], program[5]]) as usize;
    let end = len.saturating_add(6).min(program.len());
    &program[6..end]
}

/// PostScript tokens of a clear-text font header: whitespace-separated
/// runs, with `%` comments skipped and each of `()<>[]{}` a token of its
/// own. A `/` starts a name token.
struct PsTokens<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Iterator for PsTokens<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        loop {
            let &b = self.bytes.get(self.at)?;
            if is_ps_whitespace(b) {
                self.at += 1;
                continue;
            }
            if b != b'%' {
                break;
            }
            while self
                .bytes
                .get(self.at)
                .is_some_and(|&b| b != b'\n' && b != b'\r')
            {
                self.at += 1;
            }
        }
        let start = self.at;
        if is_ps_delimiter(self.bytes[start]) {
            self.at += 1;
            return Some(&self.bytes[start..self.at]);
        }
        self.at += 1;
        while self
            .bytes
            .get(self.at)
            .is_some_and(|&b| !is_ps_whitespace(b) && !is_ps_delimiter(b) && b != b'/')
        {
            self.at += 1;
        }
        Some(&self.bytes[start..self.at])
    }
}

fn is_ps_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0C' | b'\0')
}

fn is_ps_delimiter(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_ansi_spot_checks() {
        assert_eq!(win_ansi(b'A'), Some('A'));
        assert_eq!(win_ansi(0x93), Some('\u{201C}')); // left double quote
        assert_eq!(win_ansi(0x80), Some('\u{20AC}')); // euro sign
        assert_eq!(win_ansi(0xE9), Some('\u{E9}')); // e acute (Latin-1)
        assert_eq!(win_ansi(0x81), None); // unassigned
        assert_eq!(win_ansi(0x0A), None); // control
    }

    #[test]
    fn win_ansi_glyph_name_spot_checks() {
        assert_eq!(win_ansi_glyph_name(0x41), Some("A"));
        assert_eq!(win_ansi_glyph_name(0x20), Some("space"));
        assert_eq!(win_ansi_glyph_name(0x27), Some("quotesingle")); // not quoteright
        assert_eq!(win_ansi_glyph_name(0x60), Some("grave")); // not quoteleft
        assert_eq!(win_ansi_glyph_name(0x80), Some("Euro"));
        assert_eq!(win_ansi_glyph_name(0x93), Some("quotedblleft"));
        assert_eq!(win_ansi_glyph_name(0xE9), Some("eacute"));
        assert_eq!(win_ansi_glyph_name(0xFF), Some("ydieresis"));
        assert_eq!(win_ansi_glyph_name(0x81), None); // unassigned
        assert_eq!(win_ansi_glyph_name(0x0A), None); // control
    }

    /// Self-verifying anchor for `win_ansi_glyph_name`: ties the name table
    /// to the pre-existing, trusted `win_ansi` (code -> Unicode) and
    /// `glyph_to_unicode` (name -> Unicode) tables. Domain equality must
    /// hold for every code, and every name must resolve to the code's
    /// Unicode value — with exactly two documented exceptions, codes that
    /// render an existing glyph rather than owning one: `0xA0` (nonbreaking
    /// space, drawn by `space`) and `0xAD` (soft hyphen, drawn by `hyphen`).
    #[test]
    fn win_ansi_glyph_name_matches_win_ansi_table() {
        assert_eq!(win_ansi_glyph_name(0xA0), Some("space"));
        assert_eq!(win_ansi_glyph_name(0xAD), Some("hyphen"));
        for code in 0u16..=255 {
            let code = code as u8;
            assert_eq!(
                win_ansi_glyph_name(code).is_some(),
                win_ansi(code).is_some(),
                "code {code:#04x}: win_ansi_glyph_name/win_ansi domain mismatch"
            );
            let Some(name) = win_ansi_glyph_name(code) else {
                continue;
            };
            let expected = match code {
                0xA0 => ' ',
                0xAD => '-',
                _ => win_ansi(code).unwrap(),
            };
            assert_eq!(
                glyph_to_unicode(name),
                Some(expected),
                "code {code:#04x} name {name:?}: glyph_to_unicode disagrees with win_ansi"
            );
        }
    }

    #[test]
    fn mac_roman_spot_checks() {
        assert_eq!(mac_roman(b'A'), Some('A'));
        assert_eq!(mac_roman(0xD0), Some('\u{2013}')); // en dash
        assert_eq!(mac_roman(0x80), Some('\u{C4}')); // A dieresis
        assert_eq!(mac_roman(0xA5), Some('\u{2022}')); // bullet
        assert_eq!(mac_roman(0xFF), Some('\u{2C7}')); // caron
        assert_eq!(mac_roman(0x00), None);
    }

    #[test]
    fn standard_spot_checks() {
        assert_eq!(standard(b'A'), Some('A'));
        assert_eq!(standard(0xA9), Some('\u{27}')); // straight apostrophe
        assert_eq!(standard(0x27), Some('\u{2019}')); // curly right quote
        assert_eq!(standard(0x60), Some('\u{2018}')); // curly left quote
        assert_eq!(standard(0xD0), Some('\u{2014}')); // em dash
        assert_eq!(standard(0x7F), None);
        assert_eq!(standard(0xA0), None); // unassigned in Standard
    }

    #[test]
    fn glyph_names_hex_forms() {
        assert_eq!(glyph_to_unicode("uni03B1"), Some('\u{3B1}'));
        assert_eq!(glyph_to_unicode("uni20AC"), Some('\u{20AC}'));
        assert_eq!(glyph_to_unicode("u1F600"), Some('\u{1F600}'));
        assert_eq!(glyph_to_unicode("u00E9"), Some('\u{E9}'));
        assert_eq!(glyph_to_unicode("uniD800"), None); // surrogate
        assert_eq!(glyph_to_unicode("uniXYZW"), None);
    }

    /// Self-verifying anchor for `standard_encoding_name`: ties the new table
    /// to the pre-existing, trusted `standard` (code -> Unicode) and
    /// `glyph_to_unicode` (name -> Unicode) tables so an authoring typo in
    /// the new table fails a test instead of silently mis-encoding a glyph.
    /// Domain equality (StandardEncoding assigns a name to exactly the codes
    /// `standard` maps to a char) must hold for every code; value agreement
    /// only where `glyph_to_unicode` also resolves the name (some names
    /// aren't in the bundled glyph-name subset).
    #[test]
    fn standard_encoding_name_matches_standard_table() {
        for code in 0u16..=255 {
            let code = code as u8;
            assert_eq!(
                standard_encoding_name(code).is_some(),
                standard(code).is_some(),
                "code {code:#04x}: standard_encoding_name/standard domain mismatch"
            );
            if let (Some(name), Some(expected)) = (standard_encoding_name(code), standard(code)) {
                if let Some(resolved) = glyph_to_unicode(name) {
                    assert_eq!(
                        resolved, expected,
                        "code {code:#04x} name {name:?}: glyph_to_unicode disagrees with standard"
                    );
                }
            }
        }
    }

    #[test]
    fn standard_encoding_name_spot_checks() {
        assert_eq!(standard_encoding_name(b'A'), Some("A"));
        assert_eq!(standard_encoding_name(0x27), Some("quoteright"));
        assert_eq!(standard_encoding_name(0x60), Some("quoteleft"));
        assert_eq!(standard_encoding_name(0xA1), Some("exclamdown"));
        assert_eq!(standard_encoding_name(0xA4), Some("fraction"));
        assert_eq!(standard_encoding_name(0xA6), Some("florin"));
        assert_eq!(standard_encoding_name(0xC1), Some("grave"));
        assert_eq!(standard_encoding_name(0xC6), Some("breve"));
        assert_eq!(standard_encoding_name(0xE1), Some("AE"));
        assert_eq!(standard_encoding_name(0xF1), Some("ae"));
        assert_eq!(standard_encoding_name(0xFB), Some("germandbls"));
        assert_eq!(standard_encoding_name(0x7F), None);
        assert_eq!(standard_encoding_name(0xA0), None);
    }

    #[test]
    fn glyph_names_letters_and_tables() {
        assert_eq!(glyph_to_unicode("A"), Some('A'));
        assert_eq!(glyph_to_unicode("z"), Some('z'));
        assert_eq!(glyph_to_unicode("alpha"), Some('\u{3B1}'));
        assert_eq!(glyph_to_unicode("eacute"), Some('\u{E9}'));
        assert_eq!(glyph_to_unicode("quotedblleft"), Some('\u{201C}'));
        assert_eq!(glyph_to_unicode("seven"), Some('7'));
        assert_eq!(glyph_to_unicode("union"), Some('\u{222A}'));
        assert_eq!(glyph_to_unicode("nosuchglyphname"), None);
    }

    #[test]
    fn glyph_text_ligatures_and_variants() {
        assert_eq!(glyph_to_text("f_i").as_deref(), Some("fi"));
        assert_eq!(glyph_to_text("f_l").as_deref(), Some("fl"));
        assert_eq!(glyph_to_text("T_h").as_deref(), Some("Th"));
        assert_eq!(glyph_to_text("f_f_i").as_deref(), Some("ffi"));
        assert_eq!(glyph_to_text("eight.oldstyle").as_deref(), Some("8"));
        assert_eq!(glyph_to_text("x.sc").as_deref(), Some("x"));
        assert_eq!(glyph_to_text("C.a").as_deref(), Some("C"));
        // Suffix stripping happens before underscore splitting.
        assert_eq!(glyph_to_text("f_i.alt").as_deref(), Some("fi"));
        assert_eq!(glyph_to_text("uni00A0").as_deref(), Some("\u{A0}"));
        assert_eq!(glyph_to_text("eacute").as_deref(), Some("\u{E9}"));
    }

    #[test]
    fn glyph_text_multi_group_uni() {
        assert_eq!(
            glyph_to_text("uni20AC0308").as_deref(),
            Some("\u{20AC}\u{0308}")
        );
        assert_eq!(glyph_to_text("uniD800DC00"), None); // surrogates never decode
    }

    #[test]
    fn glyph_text_rejects_unknowns() {
        assert_eq!(glyph_to_text(".notdef"), None);
        assert_eq!(glyph_to_text(""), None);
        assert_eq!(glyph_to_text("glorp"), None);
        // Every component must resolve, or the whole name is unknown.
        assert_eq!(glyph_to_text("f_glorp"), None);
        assert_eq!(glyph_to_text("f__i"), None);
    }

    /// The math and symbol names a TeX-produced document's fonts carry in
    /// their built-in encodings are all in the Adobe Glyph List, as are the
    /// Hebrew names whose text is two scalars.
    #[test]
    fn glyph_names_cover_the_full_adobe_glyph_list() {
        assert_eq!(glyph_to_unicode("universal"), Some('\u{2200}'));
        assert_eq!(glyph_to_unicode("existential"), Some('\u{2203}'));
        assert_eq!(glyph_to_unicode("emptyset"), Some('\u{2205}'));
        assert_eq!(glyph_to_unicode("copyright"), Some('\u{A9}'));
        assert_eq!(glyph_to_unicode("ff"), Some('\u{FB00}'));
        assert_eq!(glyph_to_unicode("afii57414"), Some('\u{0626}'));
        assert_eq!(
            glyph_to_text("dalethatafpatah").as_deref(),
            Some("\u{05D3}\u{05B2}")
        );
        assert_eq!(glyph_to_unicode("dalethatafpatah"), None);
    }

    /// Names the Computer Modern symbol fonts use that the Adobe Glyph
    /// List never adopted.
    #[test]
    fn tex_math_glyph_names_resolve() {
        assert_eq!(glyph_to_unicode("owner"), Some('\u{220B}'));
        assert_eq!(glyph_to_unicode("arrowbothv"), Some('\u{2195}'));
        assert_eq!(glyph_to_unicode("angbracketleft"), Some('\u{27E8}'));
        assert_eq!(glyph_to_unicode("lessmuch"), Some('\u{226A}'));
    }

    #[test]
    fn type1_program_encoding_reads_dup_put_entries() {
        let program: &[u8] = b"%!PS-AdobeFont-1.0: CMSY10\n/FontName /CMSY10 def\n\
            /Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\n\
            dup 56 /universal put\ndup 169 /copyright put\nreadonly def\n\
            currentdict end\ncurrentfile eexec\n\x80\x01dup 57 /existential put";
        let table = type1_builtin_encoding(program).expect("an /Encoding is present");
        assert_eq!(table[56].as_deref(), Some("universal"));
        assert_eq!(table[169].as_deref(), Some("copyright"));
        assert_eq!(table[57], None, "nothing past eexec is read");
        assert_eq!(table[0], None);
    }

    #[test]
    fn type1_program_encoding_expands_the_standard_token() {
        let program: &[u8] = b"/Encoding StandardEncoding def\ncurrentfile eexec\n";
        let table = type1_builtin_encoding(program).unwrap();
        assert_eq!(table[0x41].as_deref(), Some("A"));
        assert_eq!(table[0x27].as_deref(), Some("quoteright"));
    }

    #[test]
    fn type1_program_encoding_reads_pfb_segments() {
        let clear: &[u8] =
            b"/Encoding 256 array\ndup 65 /alpha put\nreadonly def\ncurrentfile eexec\n";
        let mut program = vec![0x80, 0x01];
        program.extend_from_slice(&(clear.len() as u32).to_le_bytes());
        program.extend_from_slice(clear);
        program.extend_from_slice(&[0x80, 0x02, 4, 0, 0, 0, 0xDE, 0xAD, 0xBE, 0xEF, 0x80, 0x03]);
        let table = type1_builtin_encoding(&program).unwrap();
        assert_eq!(table[65].as_deref(), Some("alpha"));
    }

    #[test]
    fn type1_program_without_an_encoding_yields_none() {
        let program: &[u8] = b"/FontName /X def\ncurrentfile eexec\n";
        assert!(type1_builtin_encoding(program).is_none());
    }
}
