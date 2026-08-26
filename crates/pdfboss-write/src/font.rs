//! The fourteen standard fonts (ISO 32000 §9.6.2.2), with WinAnsi text
//! encoding and AFM metrics from `pdfboss-encoding`. Text in these faces
//! needs no embedded font program — every conforming reader carries them.

use pdfboss_core::Dict;

use crate::error::Result;

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
        todo!("base font of {self:?}")
    }

    /// Parses a PostScript base font name back to the variant.
    pub fn from_base_font(name: &str) -> Option<Standard14> {
        todo!("from base font {name:?}")
    }

    /// Encodes text to font code bytes: WinAnsi for the twelve text faces,
    /// the font-specific built-in encoding for Symbol and ZapfDingbats.
    /// A character without a code is an [`Error::Unencodable`]
    /// (crate::Error::Unencodable) — never silently dropped or replaced.
    pub fn encode(self, text: &str) -> Result<Vec<u8>> {
        todo!("encode {text:?} for {self:?}")
    }

    /// Advance width of one character in units per 1000 of font size, from
    /// the AFM metrics. `None` when the character has no code or metric.
    pub fn width(self, ch: char) -> Option<f32> {
        todo!("width of {ch:?} in {self:?}")
    }

    /// Width of a whole string at `size`, in text-space units. Errors on
    /// unencodable characters, like [`encode`](Standard14::encode).
    pub fn text_width(self, text: &str, size: f32) -> Result<f32> {
        todo!("width of {text:?} at {size} in {self:?}")
    }

    /// The font dictionary describing this face (`/Type /Font`,
    /// `/Subtype /Type1`, `/BaseFont`, and `/Encoding /WinAnsiEncoding`
    /// for the twelve text faces).
    pub(crate) fn font_dict(self) -> Dict {
        todo!("font dict for {self:?}")
    }
}
