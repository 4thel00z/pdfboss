//! The fourteen standard fonts (ISO 32000 §9.6.2.2), with WinAnsi text
//! encoding and AFM metrics from `pdfboss-encoding`. Text in these faces
//! needs no embedded font program — every conforming reader carries them.

use pdfboss_core::{Dict, Name, Object};

use crate::error::{Error, Result};

/// One of the fourteen standard fonts every PDF consumer provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Standard14 {
    /// Helvetica.
    Helvetica,
    /// Helvetica-Bold.
    HelveticaBold,
    /// Helvetica-Oblique.
    HelveticaOblique,
    /// Helvetica-BoldOblique.
    HelveticaBoldOblique,
    /// Times-Roman.
    TimesRoman,
    /// Times-Bold.
    TimesBold,
    /// Times-Italic.
    TimesItalic,
    /// Times-BoldItalic.
    TimesBoldItalic,
    /// Courier.
    Courier,
    /// Courier-Bold.
    CourierBold,
    /// Courier-Oblique.
    CourierOblique,
    /// Courier-BoldOblique.
    CourierBoldOblique,
    /// Symbol (font-specific encoding).
    Symbol,
    /// ZapfDingbats (font-specific encoding).
    ZapfDingbats,
}

impl Standard14 {
    /// All fourteen, in ISO 32000 listing order.
    pub const ALL: [Standard14; 14] = [
        Standard14::Helvetica,
        Standard14::HelveticaBold,
        Standard14::HelveticaOblique,
        Standard14::HelveticaBoldOblique,
        Standard14::TimesRoman,
        Standard14::TimesBold,
        Standard14::TimesItalic,
        Standard14::TimesBoldItalic,
        Standard14::Courier,
        Standard14::CourierBold,
        Standard14::CourierOblique,
        Standard14::CourierBoldOblique,
        Standard14::Symbol,
        Standard14::ZapfDingbats,
    ];

    /// The PostScript base font name, e.g. `"Helvetica-Bold"`.
    pub fn base_font(self) -> &'static str {
        match self {
            Standard14::Helvetica => "Helvetica",
            Standard14::HelveticaBold => "Helvetica-Bold",
            Standard14::HelveticaOblique => "Helvetica-Oblique",
            Standard14::HelveticaBoldOblique => "Helvetica-BoldOblique",
            Standard14::TimesRoman => "Times-Roman",
            Standard14::TimesBold => "Times-Bold",
            Standard14::TimesItalic => "Times-Italic",
            Standard14::TimesBoldItalic => "Times-BoldItalic",
            Standard14::Courier => "Courier",
            Standard14::CourierBold => "Courier-Bold",
            Standard14::CourierOblique => "Courier-Oblique",
            Standard14::CourierBoldOblique => "Courier-BoldOblique",
            Standard14::Symbol => "Symbol",
            Standard14::ZapfDingbats => "ZapfDingbats",
        }
    }

    /// Parses a PostScript base font name back to the variant. Exact names
    /// only — aliases and subset-tagged forms are the reading side's
    /// business, not the writer's.
    pub fn from_base_font(name: &str) -> Option<Standard14> {
        Standard14::ALL
            .into_iter()
            .find(|font| font.base_font() == name)
    }

    /// Whether this face encodes text as WinAnsi (the twelve text faces)
    /// rather than a font-specific built-in encoding.
    fn is_win_ansi(self) -> bool {
        !matches!(self, Standard14::Symbol | Standard14::ZapfDingbats)
    }

    /// The WinAnsi code for `ch`, scanning codes in ascending order so a
    /// duplicated character would resolve to its lowest code. Symbol and
    /// ZapfDingbats have no encoding tables yet, so every character is an
    /// [`Error::Unencodable`] there.
    fn encode_char(self, ch: char) -> Result<u8> {
        if !self.is_win_ansi() {
            return Err(Error::Unencodable {
                ch,
                font: self.base_font(),
            });
        }
        (0u8..=255)
            .find(|&code| pdfboss_encoding::win_ansi(code) == Some(ch))
            .ok_or(Error::Unencodable {
                ch,
                font: self.base_font(),
            })
    }

    /// Encodes text to font code bytes: WinAnsi for the twelve text faces,
    /// the font-specific built-in encoding for Symbol and ZapfDingbats.
    /// A character without a code is an [`Error::Unencodable`]
    /// (crate::Error::Unencodable) — never silently dropped or replaced.
    /// Symbol and ZapfDingbats currently reject every character: their
    /// encoding tables come with a later phase.
    pub fn encode(self, text: &str) -> Result<Vec<u8>> {
        text.chars().map(|ch| self.encode_char(ch)).collect()
    }

    /// Advance width of one character in units per 1000 of font size, from
    /// the AFM metrics. `None` when the character has no code or metric.
    pub fn width(self, ch: char) -> Option<f32> {
        let code = self.encode_char(ch).ok()?;
        let glyph = pdfboss_encoding::win_ansi_glyph_name(code)?;
        pdfboss_encoding::standard_14_width(self.base_font(), glyph)
    }

    /// Width of a whole string at `size`, in text-space units. Errors on
    /// unencodable characters, like [`encode`](Standard14::encode); an
    /// encodable character whose glyph has no AFM metric is an
    /// [`Error::Other`] naming the glyph. No kerning — the AFM kern pairs
    /// are not bundled, so this is the sum of bare advance widths.
    pub fn text_width(self, text: &str, size: f32) -> Result<f32> {
        let mut sum = 0.0f32;
        for ch in text.chars() {
            let code = self.encode_char(ch)?;
            let glyph = pdfboss_encoding::win_ansi_glyph_name(code).ok_or_else(|| {
                Error::Other(format!("code {code:#04x} has no WinAnsi glyph name"))
            })?;
            let width =
                pdfboss_encoding::standard_14_width(self.base_font(), glyph).ok_or_else(|| {
                    Error::Other(format!(
                        "{} has no metric for glyph {glyph:?}",
                        self.base_font()
                    ))
                })?;
            sum += width;
        }
        Ok(sum * size / 1000.0)
    }

    /// The font dictionary describing this face (`/Type /Font`,
    /// `/Subtype /Type1`, `/BaseFont`, and `/Encoding /WinAnsiEncoding`
    /// for the twelve text faces).
    pub(crate) fn font_dict(self) -> Dict {
        let mut dict = Dict::new();
        dict.insert(Name("Type".into()), Object::Name(Name("Font".into())));
        dict.insert(Name("Subtype".into()), Object::Name(Name("Type1".into())));
        dict.insert(
            Name("BaseFont".into()),
            Object::Name(Name(self.base_font().into())),
        );
        if self.is_win_ansi() {
            dict.insert(
                Name("Encoding".into()),
                Object::Name(Name("WinAnsiEncoding".into())),
            );
        }
        dict
    }
}

#[cfg(test)]
mod tests {
    use pdfboss_encoding::standard_14_width;

    use super::*;
    use crate::error::Error;

    #[test]
    fn base_font_round_trips_all_fourteen() {
        for font in Standard14::ALL {
            assert_eq!(Standard14::from_base_font(font.base_font()), Some(font));
        }
        assert_eq!(Standard14::Helvetica.base_font(), "Helvetica");
        assert_eq!(
            Standard14::HelveticaBoldOblique.base_font(),
            "Helvetica-BoldOblique"
        );
        assert_eq!(Standard14::TimesRoman.base_font(), "Times-Roman");
        assert_eq!(Standard14::TimesBoldItalic.base_font(), "Times-BoldItalic");
        assert_eq!(Standard14::CourierOblique.base_font(), "Courier-Oblique");
        assert_eq!(Standard14::Symbol.base_font(), "Symbol");
        assert_eq!(Standard14::ZapfDingbats.base_font(), "ZapfDingbats");
        assert_eq!(Standard14::from_base_font("helvetica"), None);
        assert_eq!(Standard14::from_base_font("Arial"), None);
        assert_eq!(Standard14::from_base_font(""), None);
    }

    #[test]
    fn encode_ascii_and_win_ansi_specials() {
        assert_eq!(Standard14::Helvetica.encode("Hi").unwrap(), b"Hi");
        assert_eq!(Standard14::TimesRoman.encode("\u{E9}").unwrap(), [0xE9]);
        assert_eq!(Standard14::Courier.encode("\u{20AC}").unwrap(), [0x80]);
        assert_eq!(Standard14::Helvetica.encode("\u{201C}").unwrap(), [0x93]);
    }

    #[test]
    fn encode_rejects_unencodable_chars() {
        match Standard14::HelveticaBold.encode("\u{2318}").unwrap_err() {
            Error::Unencodable { ch, font } => {
                assert_eq!(ch, '\u{2318}');
                assert_eq!(font, "Helvetica-Bold");
            }
            other => panic!("expected Unencodable, got {other:?}"),
        }
    }

    #[test]
    fn symbol_faces_reject_every_char_for_now() {
        for font in [Standard14::Symbol, Standard14::ZapfDingbats] {
            assert!(matches!(
                font.encode("a"),
                Err(Error::Unencodable { ch: 'a', .. })
            ));
            assert_eq!(font.width('a'), None);
        }
        assert_eq!(Standard14::Symbol.encode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn width_matches_direct_afm_lookups() {
        let font = Standard14::Helvetica;
        for (ch, glyph) in [('H', "H"), ('e', "e"), ('l', "l"), ('o', "o")] {
            assert_eq!(font.width(ch), standard_14_width("Helvetica", glyph));
            assert!(font.width(ch).is_some());
        }
        assert_eq!(font.width('\u{2318}'), None); // unencodable
        assert_eq!(font.width('\u{20AC}'), None); // pre-Euro AFMs carry no metric
    }

    #[test]
    fn text_width_scales_by_size() {
        let font = Standard14::TimesBold;
        let sum: f32 = "Hello".chars().map(|ch| font.width(ch).unwrap()).sum();
        assert_eq!(font.text_width("Hello", 12.0).unwrap(), sum * 12.0 / 1000.0);
        assert_eq!(
            font.text_width("Hello", 1000.0).unwrap(),
            font.text_width("Hello", 500.0).unwrap() * 2.0
        );
        assert_eq!(font.text_width("", 12.0).unwrap(), 0.0);
        assert!(matches!(
            font.text_width("a\u{2318}", 12.0),
            Err(Error::Unencodable { ch: '\u{2318}', .. })
        ));
        assert!(matches!(
            font.text_width("\u{20AC}", 12.0),
            Err(Error::Other(msg)) if msg.contains("Euro")
        ));
    }

    #[test]
    fn font_dict_win_ansi_and_symbol() {
        let dict = Standard14::HelveticaOblique.font_dict();
        assert_eq!(dict.len(), 4);
        assert_eq!(dict.get_name("Type").map(|n| n.0.as_str()), Some("Font"));
        assert_eq!(
            dict.get_name("Subtype").map(|n| n.0.as_str()),
            Some("Type1")
        );
        assert_eq!(
            dict.get_name("BaseFont").map(|n| n.0.as_str()),
            Some("Helvetica-Oblique")
        );
        assert_eq!(
            dict.get_name("Encoding").map(|n| n.0.as_str()),
            Some("WinAnsiEncoding")
        );

        let dict = Standard14::Symbol.font_dict();
        assert_eq!(dict.len(), 3);
        assert_eq!(
            dict.get_name("BaseFont").map(|n| n.0.as_str()),
            Some("Symbol")
        );
        assert!(dict.get("Encoding").is_none());
    }
}
