//! Shared PDF font-encoding tables (WinAnsi / MacRoman / Standard, from
//! ISO 32000 Appendix D) and a bundled glyph-name-to-Unicode subset, consumed
//! by the pdfboss text-extraction and rendering crates.

mod afm;
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
/// `uniXXXX` and `uXXXX`–`uXXXXXX` hex forms, single ASCII letters, and a
/// bundled subset of the standard glyph list.
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
    GLYPH_TABLES
        .iter()
        .flat_map(|t| t.iter())
        .find(|&&(n, _)| n == name)
        .map(|&(_, u)| u)
}

/// All bundled glyph-name tables, searched in order.
const GLYPH_TABLES: [&[(&str, char)]; 5] = [
    GLYPHS_ASCII,
    GLYPHS_LATIN1,
    GLYPHS_PUNCT,
    GLYPHS_GREEK,
    GLYPHS_MISC,
];

/// Names for the ASCII range (letters are handled separately).
const GLYPHS_ASCII: &[(&str, char)] = &[
    ("space", ' '),
    ("exclam", '!'),
    ("quotedbl", '"'),
    ("numbersign", '#'),
    ("dollar", '$'),
    ("percent", '%'),
    ("ampersand", '&'),
    ("quotesingle", '\''),
    ("parenleft", '('),
    ("parenright", ')'),
    ("asterisk", '*'),
    ("plus", '+'),
    ("comma", ','),
    ("hyphen", '-'),
    ("period", '.'),
    ("slash", '/'),
    ("zero", '0'),
    ("one", '1'),
    ("two", '2'),
    ("three", '3'),
    ("four", '4'),
    ("five", '5'),
    ("six", '6'),
    ("seven", '7'),
    ("eight", '8'),
    ("nine", '9'),
    ("colon", ':'),
    ("semicolon", ';'),
    ("less", '<'),
    ("equal", '='),
    ("greater", '>'),
    ("question", '?'),
    ("at", '@'),
    ("bracketleft", '['),
    ("backslash", '\\'),
    ("bracketright", ']'),
    ("asciicircum", '^'),
    ("underscore", '_'),
    ("grave", '`'),
    ("braceleft", '{'),
    ("bar", '|'),
    ("braceright", '}'),
    ("asciitilde", '~'),
];

/// Names for the Latin-1 supplement.
const GLYPHS_LATIN1: &[(&str, char)] = &[
    ("exclamdown", '\u{A1}'),
    ("cent", '\u{A2}'),
    ("sterling", '\u{A3}'),
    ("currency", '\u{A4}'),
    ("yen", '\u{A5}'),
    ("brokenbar", '\u{A6}'),
    ("section", '\u{A7}'),
    ("dieresis", '\u{A8}'),
    ("copyright", '\u{A9}'),
    ("ordfeminine", '\u{AA}'),
    ("guillemotleft", '\u{AB}'),
    ("logicalnot", '\u{AC}'),
    ("registered", '\u{AE}'),
    ("macron", '\u{AF}'),
    ("degree", '\u{B0}'),
    ("plusminus", '\u{B1}'),
    ("twosuperior", '\u{B2}'),
    ("threesuperior", '\u{B3}'),
    ("acute", '\u{B4}'),
    ("mu", '\u{B5}'),
    ("paragraph", '\u{B6}'),
    ("periodcentered", '\u{B7}'),
    ("cedilla", '\u{B8}'),
    ("onesuperior", '\u{B9}'),
    ("ordmasculine", '\u{BA}'),
    ("guillemotright", '\u{BB}'),
    ("onequarter", '\u{BC}'),
    ("onehalf", '\u{BD}'),
    ("threequarters", '\u{BE}'),
    ("questiondown", '\u{BF}'),
    ("Agrave", '\u{C0}'),
    ("Aacute", '\u{C1}'),
    ("Acircumflex", '\u{C2}'),
    ("Atilde", '\u{C3}'),
    ("Adieresis", '\u{C4}'),
    ("Aring", '\u{C5}'),
    ("AE", '\u{C6}'),
    ("Ccedilla", '\u{C7}'),
    ("Egrave", '\u{C8}'),
    ("Eacute", '\u{C9}'),
    ("Ecircumflex", '\u{CA}'),
    ("Edieresis", '\u{CB}'),
    ("Igrave", '\u{CC}'),
    ("Iacute", '\u{CD}'),
    ("Icircumflex", '\u{CE}'),
    ("Idieresis", '\u{CF}'),
    ("Eth", '\u{D0}'),
    ("Ntilde", '\u{D1}'),
    ("Ograve", '\u{D2}'),
    ("Oacute", '\u{D3}'),
    ("Ocircumflex", '\u{D4}'),
    ("Otilde", '\u{D5}'),
    ("Odieresis", '\u{D6}'),
    ("multiply", '\u{D7}'),
    ("Oslash", '\u{D8}'),
    ("Ugrave", '\u{D9}'),
    ("Uacute", '\u{DA}'),
    ("Ucircumflex", '\u{DB}'),
    ("Udieresis", '\u{DC}'),
    ("Yacute", '\u{DD}'),
    ("Thorn", '\u{DE}'),
    ("germandbls", '\u{DF}'),
    ("agrave", '\u{E0}'),
    ("aacute", '\u{E1}'),
    ("acircumflex", '\u{E2}'),
    ("atilde", '\u{E3}'),
    ("adieresis", '\u{E4}'),
    ("aring", '\u{E5}'),
    ("ae", '\u{E6}'),
    ("ccedilla", '\u{E7}'),
    ("egrave", '\u{E8}'),
    ("eacute", '\u{E9}'),
    ("ecircumflex", '\u{EA}'),
    ("edieresis", '\u{EB}'),
    ("igrave", '\u{EC}'),
    ("iacute", '\u{ED}'),
    ("icircumflex", '\u{EE}'),
    ("idieresis", '\u{EF}'),
    ("eth", '\u{F0}'),
    ("ntilde", '\u{F1}'),
    ("ograve", '\u{F2}'),
    ("oacute", '\u{F3}'),
    ("ocircumflex", '\u{F4}'),
    ("otilde", '\u{F5}'),
    ("odieresis", '\u{F6}'),
    ("divide", '\u{F7}'),
    ("oslash", '\u{F8}'),
    ("ugrave", '\u{F9}'),
    ("uacute", '\u{FA}'),
    ("ucircumflex", '\u{FB}'),
    ("udieresis", '\u{FC}'),
    ("yacute", '\u{FD}'),
    ("thorn", '\u{FE}'),
    ("ydieresis", '\u{FF}'),
];

/// Typographic punctuation, ligatures, and accents.
const GLYPHS_PUNCT: &[(&str, char)] = &[
    ("quoteleft", '\u{2018}'),
    ("quoteright", '\u{2019}'),
    ("quotesinglbase", '\u{201A}'),
    ("quotedblleft", '\u{201C}'),
    ("quotedblright", '\u{201D}'),
    ("quotedblbase", '\u{201E}'),
    ("endash", '\u{2013}'),
    ("emdash", '\u{2014}'),
    ("bullet", '\u{2022}'),
    ("ellipsis", '\u{2026}'),
    ("dagger", '\u{2020}'),
    ("daggerdbl", '\u{2021}'),
    ("perthousand", '\u{2030}'),
    ("guilsinglleft", '\u{2039}'),
    ("guilsinglright", '\u{203A}'),
    ("fraction", '\u{2044}'),
    ("minus", '\u{2212}'),
    ("florin", '\u{192}'),
    ("Euro", '\u{20AC}'),
    ("trademark", '\u{2122}'),
    ("fi", '\u{FB01}'),
    ("fl", '\u{FB02}'),
    ("ff", '\u{FB00}'),
    ("ffi", '\u{FB03}'),
    ("ffl", '\u{FB04}'),
    ("circumflex", '\u{2C6}'),
    ("caron", '\u{2C7}'),
    ("breve", '\u{2D8}'),
    ("dotaccent", '\u{2D9}'),
    ("ring", '\u{2DA}'),
    ("ogonek", '\u{2DB}'),
    ("tilde", '\u{2DC}'),
    ("hungarumlaut", '\u{2DD}'),
    ("OE", '\u{152}'),
    ("oe", '\u{153}'),
    ("Scaron", '\u{160}'),
    ("scaron", '\u{161}'),
    ("Zcaron", '\u{17D}'),
    ("zcaron", '\u{17E}'),
    ("Ydieresis", '\u{178}'),
    ("Lslash", '\u{141}'),
    ("lslash", '\u{142}'),
    ("dotlessi", '\u{131}'),
    ("nbspace", '\u{A0}'),
    ("sfthyphen", '\u{AD}'),
];

/// Greek letters (per the glyph list, `Delta`/`Omega`/`mu` map to their
/// technical-symbol codepoints; `mu` lives in the Latin-1 table).
const GLYPHS_GREEK: &[(&str, char)] = &[
    ("Alpha", '\u{391}'),
    ("Beta", '\u{392}'),
    ("Gamma", '\u{393}'),
    ("Delta", '\u{2206}'),
    ("Epsilon", '\u{395}'),
    ("Zeta", '\u{396}'),
    ("Eta", '\u{397}'),
    ("Theta", '\u{398}'),
    ("Iota", '\u{399}'),
    ("Kappa", '\u{39A}'),
    ("Lambda", '\u{39B}'),
    ("Mu", '\u{39C}'),
    ("Nu", '\u{39D}'),
    ("Xi", '\u{39E}'),
    ("Omicron", '\u{39F}'),
    ("Pi", '\u{3A0}'),
    ("Rho", '\u{3A1}'),
    ("Sigma", '\u{3A3}'),
    ("Tau", '\u{3A4}'),
    ("Upsilon", '\u{3A5}'),
    ("Phi", '\u{3A6}'),
    ("Chi", '\u{3A7}'),
    ("Psi", '\u{3A8}'),
    ("Omega", '\u{2126}'),
    ("alpha", '\u{3B1}'),
    ("beta", '\u{3B2}'),
    ("gamma", '\u{3B3}'),
    ("delta", '\u{3B4}'),
    ("epsilon", '\u{3B5}'),
    ("zeta", '\u{3B6}'),
    ("eta", '\u{3B7}'),
    ("theta", '\u{3B8}'),
    ("iota", '\u{3B9}'),
    ("kappa", '\u{3BA}'),
    ("lambda", '\u{3BB}'),
    ("nu", '\u{3BD}'),
    ("xi", '\u{3BE}'),
    ("omicron", '\u{3BF}'),
    ("pi", '\u{3C0}'),
    ("rho", '\u{3C1}'),
    ("sigma", '\u{3C3}'),
    ("sigma1", '\u{3C2}'),
    ("tau", '\u{3C4}'),
    ("upsilon", '\u{3C5}'),
    ("phi", '\u{3C6}'),
    ("chi", '\u{3C7}'),
    ("psi", '\u{3C8}'),
    ("omega", '\u{3C9}'),
];

/// Mathematical and miscellaneous symbols.
const GLYPHS_MISC: &[(&str, char)] = &[
    ("infinity", '\u{221E}'),
    ("notequal", '\u{2260}'),
    ("lessequal", '\u{2264}'),
    ("greaterequal", '\u{2265}'),
    ("partialdiff", '\u{2202}'),
    ("summation", '\u{2211}'),
    ("product", '\u{220F}'),
    ("integral", '\u{222B}'),
    ("radical", '\u{221A}'),
    ("approxequal", '\u{2248}'),
    ("equivalence", '\u{2261}'),
    ("element", '\u{2208}'),
    ("intersection", '\u{2229}'),
    ("union", '\u{222A}'),
    ("arrowleft", '\u{2190}'),
    ("arrowup", '\u{2191}'),
    ("arrowright", '\u{2192}'),
    ("arrowdown", '\u{2193}'),
    ("arrowboth", '\u{2194}'),
    ("lozenge", '\u{25CA}'),
    ("apple", '\u{F8FF}'),
];

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
}
